//! Turning [`SpectrumFrame`]s into a few smoothly moving levels for the Now Playing
//! views (Waves, CD + Spectrum), and drawing smooth curves through points.

use gtk::gsk;
use lio_playback::SpectrumFrame;

/// Levels per frame, spaced evenly in pitch from `MIN_HZ` to `MAX_HZ`, lowest first.
pub const POINTS: usize = 24;
const MIN_HZ: f32 = 40.0;
const MAX_HZ: f32 = 12_000.0;
/// A point's power (summed over its bands) maps from 0.0 at `QUIET_DB` to 1.0 at
/// `LOUD_DB`. Each point spans the same share of pitch, so pink noise (and most music,
/// roughly) reads evenly.
const QUIET_DB: f32 = -60.0;
const LOUD_DB: f32 = -20.0;

pub type Levels = [f32; POINTS];

/// Levels (0.0–1.0) for `frame`, lowest pitch first: the power in each point's pitch
/// range, scaled between `QUIET_DB` and `LOUD_DB`, eased, then blurred into its
/// neighbours `blur_passes` times so loud pitches swell broadly instead of spiking.
pub fn targets(frame: &SpectrumFrame, blur_passes: usize) -> Levels {
    let mut out = point_db(frame);
    for slot in &mut out {
        let v = ((*slot - QUIET_DB) / (LOUD_DB - QUIET_DB)).clamp(0.0, 1.0);
        *slot = v * v * (3.0 - 2.0 * v);
    }
    blur(&mut out, blur_passes);
    out
}

/// Power (dB) in each point's pitch range, lowest pitch first.
fn point_db(frame: &SpectrumFrame) -> Levels {
    let bands = frame.db.len();
    let mut out = [lio_playback::SPECTRUM_FLOOR_DB; POINTS];
    if bands == 0 || frame.nyquist <= 0.0 {
        return out;
    }
    let band_hz = frame.nyquist / bands as f32;
    let hz_at = |i: usize| MIN_HZ * (MAX_HZ / MIN_HZ).powf(i as f32 / POINTS as f32);
    for (i, slot) in out.iter_mut().enumerate() {
        let lo = ((hz_at(i) / band_hz) as usize).min(bands - 1);
        let hi = ((hz_at(i + 1) / band_hz) as usize).clamp(lo, bands - 1);
        // At least three bands per point: a single FFT band's level jumps around a lot
        // from frame to frame, which matters in the bass where points are narrow.
        let (lo, hi) = if hi - lo < 2 {
            let mid = (lo + hi) / 2;
            (mid.saturating_sub(1), (mid + 1).min(bands - 1))
        } else {
            (lo, hi)
        };
        let power: f32 = frame.db[lo..=hi]
            .iter()
            .map(|db| 10f32.powf(db / 10.0))
            .sum();
        *slot = 10.0 * power.max(f32::MIN_POSITIVE).log10();
    }
    out
}

/// Levels that show what *stands out*, rather than how loud each pitch is: the way
/// popular visualisers make a spectrum readable.
///
/// - Each point learns its usual level and how much it normally jitters around it, much
///   like the "common mode" of audio-reactive-led-strip. Only what rises clearly beyond
///   that jitter shows: steady sound such as noise or a held pad settles to nothing
///   (even in the bass, where few FFT bins make levels jumpy), while a kick, a bass
///   note or a vocal phrase pushes out.
/// - What stands out is scaled to a slowly decaying recent peak (like cava's
///   "autosens"), so quiet and loud songs both fill the shape, with a minimum so near
///   silence isn't blown up.
/// - Each point then lifts its neighbours by a falloff per point (cava's "Monstercat
///   smoothing"), giving pointed peaks with tapering sides instead of flat slabs.
pub struct Normaliser {
    /// Usual level per point (dB).
    usual: Option<Levels>,
    /// Usual distance from it (dB, mean absolute deviation).
    jitter: Levels,
    peak_db: f32,
}

/// How quickly the usual level follows the music up and down, in seconds: a held
/// sound fades over a couple of seconds; after a hit it settles back quickly.
const USUAL_RISE_S: f64 = 2.0;
const USUAL_FALL_S: f64 = 0.5;
/// How quickly the usual jitter is learned, in seconds, and what it starts at (dB).
const JITTER_S: f64 = 1.5;
const START_JITTER_DB: f32 = 3.0;
/// A point stands out by how far it rises beyond this many times its usual jitter,
/// plus a small margin (dB).
const JITTER_TIMES: f32 = 2.5;
const STANDOUT_MARGIN_DB: f32 = 1.5;
/// The recent peak decays over this many seconds, and never counts as less than
/// `MIN_PEAK_DB`.
const PEAK_DECAY_S: f64 = 4.0;
const MIN_PEAK_DB: f32 = 12.0;
/// Each point lifts its neighbours to its level divided by this per point of distance.
const MONSTERCAT: f32 = 1.5;

impl Default for Normaliser {
    fn default() -> Self {
        Self {
            usual: None,
            jitter: [START_JITTER_DB; POINTS],
            peak_db: MIN_PEAK_DB,
        }
    }
}

impl Normaliser {
    /// Levels (0.0–1.0, lowest pitch first) for `frame`, `dt` seconds after the last.
    pub fn update(&mut self, frame: &SpectrumFrame, dt: f64) -> Levels {
        let db = point_db(frame);
        let usual = self.usual.get_or_insert(db);
        let ease = |seconds: f64| (1.0 - (-dt / seconds).exp()) as f32;
        let (rise, fall, learn) = (ease(USUAL_RISE_S), ease(USUAL_FALL_S), ease(JITTER_S));
        let mut standout = [0.0; POINTS];
        for (i, out) in standout.iter_mut().enumerate() {
            let d = db[i] - usual[i];
            *out = (d - JITTER_TIMES * self.jitter[i] - STANDOUT_MARGIN_DB).max(0.0);
            usual[i] += d * if d > 0.0 { rise } else { fall };
            self.jitter[i] += (d.abs() - self.jitter[i]) * learn;
        }
        let loudest = standout.iter().copied().fold(0.0, f32::max);
        let decay = (-dt / PEAK_DECAY_S).exp() as f32;
        self.peak_db = (self.peak_db * decay).max(loudest).max(MIN_PEAK_DB);
        let mut levels = standout.map(|d| (d / self.peak_db).min(1.0));
        monstercat(&mut levels);
        levels
    }

    /// Forgets what has been learned (new track, seek).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Lifts each point's neighbours to at least its level divided by `MONSTERCAT` per point
/// of distance.
fn monstercat(levels: &mut Levels) {
    let source = *levels;
    for (i, level) in source.iter().enumerate() {
        for (j, other) in levels.iter_mut().enumerate() {
            let falloff = MONSTERCAT.powi(i.abs_diff(j) as i32);
            *other = other.max(level / falloff);
        }
    }
}

/// Spreads each point into its neighbours with a [1, 2, 1] kernel. The first point's
/// neighbour on the far side is its own mirror image; the last repeats itself.
fn blur(levels: &mut Levels, passes: usize) {
    for _ in 0..passes {
        let prev = *levels;
        for (i, level) in levels.iter_mut().enumerate() {
            let left = if i == 0 { prev[1] } else { prev[i - 1] };
            let right = prev[(i + 1).min(POINTS - 1)];
            *level = (left + 2.0 * prev[i] + right) / 4.0;
        }
    }
}

/// Moves `levels` towards `targets` with time constants `attack_s` (rising) and
/// `release_s` (falling), in seconds.
pub fn smooth(levels: &mut Levels, targets: &Levels, dt: f64, attack_s: f64, release_s: f64) {
    let attack = (1.0 - (-dt / attack_s).exp()) as f32;
    let release = (1.0 - (-dt / release_s).exp()) as f32;
    for (level, target) in levels.iter_mut().zip(targets) {
        let k = if target > level { attack } else { release };
        *level += (target - *level) * k;
    }
}

/// `levels` interpolated at `u` (0.0 = first point, 1.0 = last point).
pub fn level_at(levels: &Levels, u: f32) -> f32 {
    let pos = u.clamp(0.0, 1.0) * (POINTS - 1) as f32;
    let i = (pos as usize).min(POINTS - 2);
    let t = pos - i as f32;
    // Cosine interpolation keeps the result smooth between points.
    let t = (1.0 - (std::f32::consts::PI * t).cos()) / 2.0;
    levels[i] + (levels[i + 1] - levels[i]) * t
}

/// A smooth curve through `points` (Catmull-Rom splines as cubic Béziers). A `closed`
/// curve joins its last point back to its first without a corner.
pub fn smooth_path(points: &[(f32, f32)], closed: bool) -> gsk::Path {
    let builder = gsk::PathBuilder::new();
    let Some(&(x0, y0)) = points.first() else {
        return builder.to_path();
    };
    let n = points.len() as isize;
    let at = |i: isize| {
        if closed {
            points[i.rem_euclid(n) as usize]
        } else {
            points[i.clamp(0, n - 1) as usize]
        }
    };
    builder.move_to(x0, y0);
    let segments = if closed { n } else { n - 1 };
    for i in 0..segments {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        builder.cubic_to(
            p1.0 + (p2.0 - p0.0) / 6.0,
            p1.1 + (p2.1 - p0.1) / 6.0,
            p2.0 - (p3.0 - p1.0) / 6.0,
            p2.1 - (p3.1 - p1.1) / 6.0,
            p2.0,
            p2.1,
        );
    }
    if closed {
        builder.close();
    }
    builder.to_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn frame_with_peak_at(hz: f32) -> SpectrumFrame {
        let nyquist = 22_050.0;
        let bands = lio_playback::SPECTRUM_BANDS as usize;
        let mut db = vec![lio_playback::SPECTRUM_FLOOR_DB; bands];
        db[(hz / (nyquist / bands as f32)) as usize] = -20.0;
        SpectrumFrame { db, nyquist }
    }

    fn peak(levels: &Levels) -> usize {
        (0..POINTS)
            .max_by(|a, b| levels[*a].total_cmp(&levels[*b]))
            .unwrap()
    }

    #[test]
    fn silence_is_zero() {
        let levels = targets(
            &SpectrumFrame {
                db: vec![lio_playback::SPECTRUM_FLOOR_DB; lio_playback::SPECTRUM_BANDS as usize],
                nyquist: 22_050.0,
            },
            6,
        );
        assert!(levels.iter().all(|l| *l == 0.0));
    }

    #[test]
    fn low_pitches_come_first() {
        let bass = targets(&frame_with_peak_at(100.0), 6);
        assert!(peak(&bass) < POINTS / 3, "100 Hz at point {}", peak(&bass));
        let treble = targets(&frame_with_peak_at(8_000.0), 6);
        assert!(
            peak(&treble) > POINTS * 2 / 3,
            "8 kHz at point {}",
            peak(&treble)
        );
    }

    /// White noise: the same power in every band.
    fn noise(db: f32) -> SpectrumFrame {
        SpectrumFrame {
            db: vec![db; lio_playback::SPECTRUM_BANDS as usize],
            nyquist: 22_050.0,
        }
    }

    fn with_bump(mut frame: SpectrumFrame, hz: f32, db: f32) -> SpectrumFrame {
        let band = (hz / (frame.nyquist / frame.db.len() as f32)) as usize;
        for b in band.saturating_sub(1)..=band + 1 {
            frame.db[b] = db;
        }
        frame
    }

    /// Noise as the spectrum element reports it: each band's level jumps around randomly
    /// from frame to frame (about 5.6 dB for a single FFT bin).
    fn jittery_noise(db: f32, seed: &mut u64) -> SpectrumFrame {
        let mut frame = noise(db);
        for band in &mut frame.db {
            // Sum of uniforms: roughly normal, standard deviation 5.6 dB.
            let mut sum = 0.0;
            for _ in 0..6 {
                *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                sum += (*seed >> 33) as f32 / (1u64 << 31) as f32 - 0.5;
            }
            *band += sum * 5.6 * 2.0_f32.sqrt();
        }
        frame
    }

    fn run(n: &mut Normaliser, frame: &SpectrumFrame, seconds: f64) -> Levels {
        let mut levels = [0.0; POINTS];
        for _ in 0..(seconds * 60.0) as usize {
            levels = n.update(frame, 1.0 / 60.0);
        }
        levels
    }

    #[test]
    fn steady_noise_settles_to_nothing() {
        let mut n = Normaliser::default();
        let levels = run(&mut n, &noise(-40.0), 10.0);
        assert!(levels.iter().all(|l| *l < 0.05), "{levels:?}");
        // Even loud noise: it's the sameness that matters, not the volume.
        let levels = run(&mut n, &noise(-10.0), 10.0);
        assert!(levels.iter().all(|l| *l < 0.05), "{levels:?}");
    }

    #[test]
    fn jittery_noise_stays_quiet_even_in_the_bass() {
        let mut n = Normaliser::default();
        let mut seed = 7;
        for _ in 0..(5.0 * 60.0) as usize {
            n.update(&jittery_noise(-40.0, &mut seed), 1.0 / 60.0);
        }
        // Over the next five seconds, what is drawn (after the display's usual
        // smoothing: quick rise, slower fall) gets no more than a small flicker, and on
        // average the shape stays all but still.
        let (mut highest, mut total) = (0.0_f32, 0.0_f32);
        let frames = (5.0 * 60.0) as usize;
        let mut shown = [0.0; POINTS];
        for _ in 0..frames {
            let levels = n.update(&jittery_noise(-40.0, &mut seed), 1.0 / 60.0);
            smooth(&mut shown, &levels, 1.0 / 60.0, 0.03, 0.18);
            let top = shown.iter().copied().fold(0.0, f32::max);
            highest = highest.max(top);
            total += top;
        }
        assert!(
            total / frames as f32 <= 0.05,
            "average {}",
            total / frames as f32
        );
        assert!(highest < 0.35, "highest {highest}");
    }

    #[test]
    fn a_kick_shows_through_jittery_noise() {
        let mut n = Normaliser::default();
        let mut seed = 11;
        for _ in 0..(5.0 * 60.0) as usize {
            n.update(&jittery_noise(-40.0, &mut seed), 1.0 / 60.0);
        }
        let mut kick = jittery_noise(-40.0, &mut seed);
        let band = (60.0 / (kick.nyquist / kick.db.len() as f32)) as usize;
        for b in band.saturating_sub(2)..=band + 2 {
            kick.db[b] = -10.0;
        }
        let levels = n.update(&kick, 1.0 / 60.0);
        let peak = (0..POINTS)
            .max_by(|a, b| levels[*a].total_cmp(&levels[*b]))
            .unwrap();
        assert!(peak < POINTS / 4 && levels[peak] > 0.8, "{levels:?}");
    }

    #[test]
    fn what_stands_out_shows_and_nothing_else() {
        let mut n = Normaliser::default();
        run(&mut n, &noise(-50.0), 5.0);
        // A bass hit over the noise.
        let levels = n.update(&with_bump(noise(-50.0), 80.0, -20.0), 1.0 / 60.0);
        let peak = (0..POINTS)
            .max_by(|a, b| levels[*a].total_cmp(&levels[*b]))
            .unwrap();
        assert!(peak < POINTS / 4, "bass at point {peak}");
        assert!(levels[peak] > 0.9);
        // Treble stays still.
        assert!(levels[POINTS - 1] < 0.05, "{levels:?}");
    }

    #[test]
    fn peaks_taper_instead_of_forming_slabs() {
        let mut n = Normaliser::default();
        run(&mut n, &noise(-50.0), 5.0);
        let levels = n.update(&with_bump(noise(-50.0), 1_000.0, -20.0), 1.0 / 60.0);
        let peak = (0..POINTS)
            .max_by(|a, b| levels[*a].total_cmp(&levels[*b]))
            .unwrap();
        // Strictly falling away from the peak on both sides for a few points.
        for d in 1..4 {
            assert!(levels[peak - d] < levels[peak - d + 1]);
            assert!(levels[peak + d] < levels[peak + d - 1]);
        }
    }

    #[test]
    fn a_held_note_fades_but_a_repeated_one_keeps_showing() {
        let held = with_bump(noise(-50.0), 200.0, -20.0);
        let mut n = Normaliser::default();
        run(&mut n, &noise(-50.0), 3.0);
        let start = n.update(&held, 1.0 / 60.0);
        let point = (0..POINTS)
            .max_by(|a, b| start[*a].total_cmp(&start[*b]))
            .unwrap();
        let later = run(&mut n, &held, 12.0);
        assert!(
            later[point] < start[point] * 0.2,
            "{} -> {}",
            start[point],
            later[point]
        );
        // The same note hit twice a second, like a kick or a bassline, still shows
        // strongly on each hit.
        let mut n = Normaliser::default();
        run(&mut n, &noise(-50.0), 3.0);
        let mut hit = 0.0_f32;
        for _ in 0..20 {
            run(&mut n, &noise(-50.0), 0.4);
            hit = n.update(&held, 1.0 / 60.0)[point];
            run(&mut n, &held, 0.1);
        }
        assert!(hit > 0.6, "hit {hit}");
    }

    #[test]
    fn rises_quickly_and_falls_slowly() {
        let mut levels = [0.0; POINTS];
        smooth(&mut levels, &[1.0; POINTS], 1.0 / 60.0, 0.06, 0.35);
        let risen = levels[0];
        let mut falling = [1.0; POINTS];
        smooth(&mut falling, &[0.0; POINTS], 1.0 / 60.0, 0.06, 0.35);
        assert!(risen > 1.0 - falling[0]);
    }
}
