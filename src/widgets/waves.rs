//! The "Waves" view for Now Playing: a block of the cover's colour with a single line
//! across its middle, a gentle wave that swells with the music and drifts outwards. Low
//! frequencies set its height near the centre, high ones towards the edges, and the two
//! halves mirror each other.

use std::cell::{Cell, RefCell};

use gettextrs::gettext;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::application::LioApplication;
use crate::widgets::spectrum_levels::{self, Levels, POINTS, level_at, smooth_path};
/// Time constants for the wave swelling with the music and settling again, in seconds.
const ATTACK_S: f64 = 0.06;
const RELEASE_S: f64 = 0.35;
/// Passes of a [1, 2, 1] blur over the points: spreads a loud pitch into a broad swell.
const BLUR_PASSES: usize = 24;
/// Share of the wave's height set by the loudest point overall, so the whole line moves
/// with the music; the rest follows where in pitch the sound is.
const OVERALL_SHARE: f32 = 0.45;
/// Whole waves from the centre to each edge.
const CYCLES: f32 = 3.0;
/// How fast the wave drifts outwards, in radians per second: a slow sway, quicker when
/// the music is louder.
const DRIFT: f64 = 1.2;
const DRIFT_LOUD: f64 = 3.0;
/// Samples drawn on each side of the centre.
const SAMPLES: usize = 96;
/// Tallest swell, as a share of the height.
const MAX_AMPLITUDE: f32 = 0.28;

mod imp {
    use super::*;

    pub struct LioWaves {
        pub colour: Cell<(u8, u8, u8)>,
        /// Smoothed height per point, centre first, 0.0–1.0.
        pub levels: RefCell<[f32; POINTS]>,
        /// The wave's drift, in radians.
        pub phase: Cell<f64>,
        /// Whether the last frame drawn was already flat.
        pub drawn_flat: Cell<bool>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub last_frame_us: Cell<i64>,
    }

    impl Default for LioWaves {
        fn default() -> Self {
            Self {
                colour: Cell::new((0x3d, 0x3d, 0x46)),
                levels: RefCell::new([0.0; POINTS]),
                phase: Cell::new(0.0),
                drawn_flat: Cell::new(false),
                tick: RefCell::default(),
                last_frame_us: Cell::new(0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioWaves {
        const NAME: &'static str = "LioWaves";
        type Type = super::LioWaves;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("waves");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for LioWaves {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj()
                .update_property(&[gtk::accessible::Property::Label(&gettext("Waves"))]);
        }
    }

    impl WidgetImpl for LioWaves {
        fn map(&self) {
            self.parent_map();
            LioApplication::default().playback().want_spectrum(true);
            self.last_frame_us.set(0);
            let id = self.obj().add_tick_callback(|w, clock| {
                w.advance(clock.frame_time());
                glib::ControlFlow::Continue
            });
            self.tick.replace(Some(id));
        }

        fn unmap(&self) {
            if let Some(id) = self.tick.take() {
                id.remove();
            }
            LioApplication::default().playback().want_spectrum(false);
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            let (r, g, b) = self.colour.get();
            let background = gdk::RGBA::new(
                f32::from(r) / 255.0,
                f32::from(g) / 255.0,
                f32::from(b) / 255.0,
                1.0,
            );
            snapshot.append_color(&background, &graphene::Rect::new(0.0, 0.0, w, h));

            let points = wave_points(&self.levels.borrow(), self.phase.get() as f32, w, h);
            let path = smooth_path(&points, false);
            let stroke = gsk::Stroke::new((w / 150.0).max(2.0));
            stroke.set_line_cap(gsk::LineCap::Round);
            stroke.set_line_join(gsk::LineJoin::Round);
            snapshot.append_stroke(&path, &stroke, &line_colour((r, g, b)));
        }
    }
}

glib::wrapper! {
    pub struct LioWaves(ObjectSubclass<imp::LioWaves>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for LioWaves {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl LioWaves {
    /// Sets the block's colour (from the cover art).
    pub fn set_colour(&self, colour: (u8, u8, u8)) {
        self.imp().colour.set(colour);
        self.queue_draw();
    }

    /// Moves the line towards the current spectrum (flat while nothing plays).
    fn advance(&self, frame_us: i64) {
        let imp = self.imp();
        let last = imp.last_frame_us.replace(frame_us);
        let dt = if last == 0 {
            1.0 / 60.0
        } else {
            ((frame_us - last) as f64 / 1e6).clamp(0.0, 0.25)
        };
        let playback = LioApplication::default().playback();
        let targets = playback
            .playing()
            .then(|| playback.spectrum())
            .flatten()
            .map_or([0.0; POINTS], |frame| {
                spectrum_levels::targets(&frame, BLUR_PASSES)
            });
        let mut levels = imp.levels.borrow_mut();
        spectrum_levels::smooth(&mut levels, &targets, dt, ATTACK_S, RELEASE_S);
        let loudness = f64::from(levels.iter().sum::<f32>() / POINTS as f32);
        let quiet = levels.iter().all(|l| *l < 1e-3);
        drop(levels);
        imp.phase
            .set((imp.phase.get() + dt * (DRIFT + DRIFT_LOUD * loudness)) % std::f64::consts::TAU);
        // A flat line doesn't move: skip redrawing it.
        if !quiet || !imp.drawn_flat.replace(quiet) {
            self.queue_draw();
        }
    }
}

/// The line from left edge to right edge: a sine of `CYCLES` waves per side drifting
/// outwards by `phase`, its height following the overall level and `levels` (centre
/// first), tapering to level at both edges.
fn wave_points(levels: &Levels, phase: f32, w: f32, h: f32) -> Vec<(f32, f32)> {
    use std::f32::consts::{FRAC_PI_2, TAU};
    let (cx, cy) = (w / 2.0, h / 2.0);
    let amplitude = h * MAX_AMPLITUDE;
    let overall = levels.iter().copied().fold(0.0, f32::max);
    (0..=2 * SAMPLES)
        .map(|s| {
            // -1.0 (left edge) to 1.0 (right edge).
            let x = s as f32 / SAMPLES as f32 - 1.0;
            // |x|, rounded off at the centre so the mirror image joins without a corner.
            let u = (x * x + 0.0016).sqrt() - 0.04;
            let local = level_at(levels, u);
            let envelope = (OVERALL_SHARE * overall + (1.0 - OVERALL_SHARE) * local)
                * (FRAC_PI_2 * x.abs()).cos();
            let y = cy - amplitude * envelope * (TAU * CYCLES * u - phase).sin();
            (cx + x * cx, y)
        })
        .collect()
}

/// White on dark colours, near-black on light ones.
fn line_colour((r, g, b): (u8, u8, u8)) -> gdk::RGBA {
    let luminance = (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0;
    if luminance > 0.6 {
        gdk::RGBA::new(0.0, 0.0, 0.0, 0.7)
    } else {
        gdk::RGBA::new(1.0, 1.0, 1.0, 0.9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lio_playback::SpectrumFrame;

    fn frame_with_peak_at(hz: f32) -> SpectrumFrame {
        let nyquist = 22_050.0;
        let bands = lio_playback::SPECTRUM_BANDS as usize;
        let mut db = vec![lio_playback::SPECTRUM_FLOOR_DB; bands];
        db[(hz / (nyquist / bands as f32)) as usize] = -20.0;
        SpectrumFrame { db, nyquist }
    }

    /// Turning points (peaks and troughs) along the line.
    fn turns(points: &[(f32, f32)]) -> usize {
        points
            .windows(3)
            .filter(|w| (w[1].1 - w[0].1) * (w[2].1 - w[1].1) < 0.0)
            .count()
    }

    #[test]
    fn silence_is_a_flat_line() {
        let points = wave_points(&[0.0; POINTS], 1.0, 400.0, 400.0);
        assert!(points.iter().all(|(_, y)| *y == 200.0));
    }

    #[test]
    fn one_loud_pitch_swells_broadly() {
        let levels = spectrum_levels::targets(&frame_with_peak_at(1_000.0), BLUR_PASSES);
        let top = levels.iter().copied().fold(0.0, f32::max);
        // Spread over several points rather than one spike...
        let wide = levels.iter().filter(|l| **l > top / 4.0).count();
        assert!(wide >= 5, "only {wide} points raised: {levels:?}");
        // ...and drawn as the same few waves as any other sound.
        let points = wave_points(&levels, 0.5, 400.0, 400.0);
        let max_turns = 2 * (2.0 * CYCLES) as usize + 2;
        assert!(turns(&points) <= max_turns, "{} turns", turns(&points));
    }

    #[test]
    fn wave_is_mirrored_smooth_and_level_at_the_edges() {
        let levels = [0.8; POINTS];
        let (w, h) = (300.0, 300.0);
        let points = wave_points(&levels, 0.7, w, h);
        let (first, last) = (points[0], *points.last().unwrap());
        assert!(first.0.abs() < 1e-3 && (first.1 - h / 2.0).abs() < 1e-3);
        assert!((last.0 - w).abs() < 1e-3 && (last.1 - h / 2.0).abs() < 1e-3);
        for (l, r) in points.iter().zip(points.iter().rev()) {
            assert!((l.0 - (w - r.0)).abs() < 1e-3 && (l.1 - r.1).abs() < 1e-3);
        }
        // No corner at the centre: the slope either side is about the same size as
        // elsewhere on the curve.
        let step = |i: usize| (points[i + 1].1 - points[i].1).abs();
        let centre = step(SAMPLES) + step(SAMPLES - 1);
        let biggest = (0..points.len() - 1).map(step).fold(0.0, f32::max);
        assert!(centre <= 2.0 * biggest);
        // A wave: it crosses the middle line several times on each side.
        let crossings = points
            .windows(2)
            .filter(|p| (p[0].1 - h / 2.0) * (p[1].1 - h / 2.0) < 0.0)
            .count();
        assert!(
            crossings >= 2 * (2.0 * CYCLES) as usize - 2,
            "{crossings} crossings"
        );
    }
}
