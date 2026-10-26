//! The "CD" views for Now Playing: the album art as a disc that turns while music plays.
//! It spins up and coasts to a stop rather than starting or stopping at once. With
//! `spectrum` set ("CD + Spectrum"), a glowing spectrum surrounds the disc: bass bulging
//! out at the sides, treble at the top and bottom, mirrored both ways.

use std::cell::{Cell, RefCell};

use gettextrs::gettext;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::application::LioApplication;
use crate::widgets::spectrum_levels::{self, Levels, POINTS, level_at, smooth_path};

/// Seconds per turn at full speed.
const TURN_S: f64 = 8.0;
/// Time constant for spinning up and coasting down, in seconds: the speed covers about
/// two thirds of the way to its target in this time.
const EASE_S: f64 = 0.7;
/// Below this speed (degrees per second) a stopping disc counts as still.
const STILL_DEG_S: f64 = 0.05;
/// Disc diameter as a share of the space, leaving room for its shadow.
const DISC_SCALE: f32 = 0.92;
/// Spindle hole and hub ring radii, as shares of the disc radius.
const HOLE: f32 = 0.07;
const HUB: f32 = 0.2;
/// With the spectrum: disc diameter as a share of the space, and the spectrum's resting
/// and loudest reach beyond the disc, as shares of half the space.
const SPECTRUM_DISC_SCALE: f32 = 0.56;
const SPECTRUM_REST: f32 = 0.025;
const SPECTRUM_REACH: f32 = 0.4;
/// Spectrum movement: snaps out on a hit, falls back a little more slowly.
const SPECTRUM_ATTACK_S: f64 = 0.03;
const SPECTRUM_RELEASE_S: f64 = 0.18;
/// Passes of a [1, 2, 1] blur around the outline (a few degrees): keeps the curves
/// clean without flattening the peaks.
const OUTLINE_BLUR_PASSES: usize = 8;
/// Points around the spectrum's outline.
const SPECTRUM_SAMPLES: usize = 160;
/// Glow around the spectrum, as a share of half the space.
const GLOW: f32 = 0.05;

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::LioCd)]
    pub struct LioCd {
        /// Surround the disc with the spectrum ("CD + Spectrum").
        #[property(get, set)]
        pub spectrum: Cell<bool>,
        /// Smoothed spectrum levels, lowest pitch first.
        pub levels: RefCell<Levels>,
        /// Turns spectra into what stands out from the music's usual level.
        pub normaliser: RefCell<spectrum_levels::Normaliser>,
        pub paintable: RefCell<Option<gdk::Paintable>>,
        /// Shown when there is no album art.
        pub colour: Cell<(u8, u8, u8)>,
        /// Degrees.
        pub angle: Cell<f64>,
        /// Degrees per second.
        pub speed: Cell<f64>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub last_frame_us: Cell<i64>,
    }

    impl Default for LioCd {
        fn default() -> Self {
            Self {
                spectrum: Cell::new(false),
                levels: RefCell::new([0.0; POINTS]),
                normaliser: RefCell::default(),
                paintable: RefCell::default(),
                colour: Cell::new((0x3d, 0x3d, 0x46)),
                angle: Cell::new(0.0),
                speed: Cell::new(0.0),
                tick: RefCell::default(),
                last_frame_us: Cell::new(0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioCd {
        const NAME: &'static str = "LioCd";
        type Type = super::LioCd;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("cd");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for LioCd {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj()
                .update_property(&[gtk::accessible::Property::Label(&gettext("Album cover"))]);
        }
    }

    impl WidgetImpl for LioCd {
        fn map(&self) {
            self.parent_map();
            if self.spectrum.get() {
                LioApplication::default().playback().want_spectrum(true);
            }
            self.last_frame_us.set(0);
            let id = self.obj().add_tick_callback(|cd, clock| {
                cd.advance(clock.frame_time());
                glib::ControlFlow::Continue
            });
            self.tick.replace(Some(id));
        }

        fn unmap(&self) {
            if let Some(id) = self.tick.take() {
                id.remove();
            }
            if self.spectrum.get() {
                LioApplication::default().playback().want_spectrum(false);
            }
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            let half = w.min(h) / 2.0;
            let centre = graphene::Point::new(w / 2.0, h / 2.0);
            let spectrum = self.spectrum.get();
            let size = 2.0
                * half
                * if spectrum {
                    SPECTRUM_DISC_SCALE
                } else {
                    DISC_SCALE
                };
            let radius = size / 2.0;
            if spectrum {
                let outline = spectrum_outline(
                    &self.levels.borrow(),
                    (centre.x(), centre.y()),
                    radius + half * SPECTRUM_REST,
                    half * SPECTRUM_REACH,
                );
                let path = smooth_path(&outline, true);
                let colour = glow_colour(self.colour.get());
                snapshot.push_blur(f64::from(half * GLOW));
                snapshot.append_fill(&path, gsk::FillRule::Winding, &colour);
                snapshot.pop();
                snapshot.append_fill(&path, gsk::FillRule::Winding, &colour);
            }
            let bounds = graphene::Rect::new(centre.x() - radius, centre.y() - radius, size, size);

            snapshot.append_outset_shadow(
                &gsk::RoundedRect::from_rect(bounds, radius),
                &gdk::RGBA::new(0.0, 0.0, 0.0, 0.35),
                0.0,
                10.0,
                0.0,
                28.0,
            );

            // The disc, with its spindle hole cut out.
            let disc = gsk::PathBuilder::new();
            disc.add_circle(&centre, radius);
            disc.add_circle(&centre, radius * HOLE);
            snapshot.push_fill(&disc.to_path(), gsk::FillRule::EvenOdd);
            snapshot.save();
            snapshot.translate(&centre);
            snapshot.rotate(self.angle.get() as f32);
            snapshot.translate(&graphene::Point::new(-radius, -radius));
            match self.paintable.borrow().as_ref() {
                Some(paintable) => draw_cover(snapshot, paintable, size),
                None => {
                    let (r, g, b) = self.colour.get();
                    snapshot.append_color(
                        &gdk::RGBA::new(
                            f32::from(r) / 255.0,
                            f32::from(g) / 255.0,
                            f32::from(b) / 255.0,
                            1.0,
                        ),
                        &graphene::Rect::new(0.0, 0.0, size, size),
                    );
                }
            }
            snapshot.restore();
            snapshot.pop();

            // A faint hub ring, like the clear centre of a disc.
            let hub = gsk::PathBuilder::new();
            hub.add_circle(&centre, radius * HUB);
            snapshot.append_stroke(
                &hub.to_path(),
                &gsk::Stroke::new((size / 300.0).max(1.0)),
                &gdk::RGBA::new(1.0, 1.0, 1.0, 0.3),
            );
        }
    }
}

glib::wrapper! {
    pub struct LioCd(ObjectSubclass<imp::LioCd>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for LioCd {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl LioCd {
    /// The album art to show (`None` shows a plain disc in the cover's colour).
    pub fn set_paintable(&self, paintable: Option<&gdk::Paintable>) {
        self.imp().paintable.replace(paintable.cloned());
        self.queue_draw();
    }

    /// Colour of the plain disc shown without album art.
    pub fn set_colour(&self, colour: (u8, u8, u8)) {
        self.imp().colour.set(colour);
        self.queue_draw();
    }

    /// Turns the disc on by one frame, easing its speed towards full while music plays
    /// (and animations are on) and towards still otherwise, and moves the spectrum.
    fn advance(&self, frame_us: i64) {
        let imp = self.imp();
        let last = imp.last_frame_us.replace(frame_us);
        let dt = if last == 0 {
            1.0 / 60.0
        } else {
            ((frame_us - last) as f64 / 1e6).clamp(0.0, 0.25)
        };
        let playback = LioApplication::default().playback();
        let animations = self.settings().is_gtk_enable_animations();
        let spinning = animations && playback.playing();
        let target = if spinning { 360.0 / TURN_S } else { 0.0 };
        let speed = ease_speed(imp.speed.get(), target, dt);
        let mut moving = speed != 0.0 || imp.speed.get() != 0.0;
        imp.speed.set(speed);
        imp.angle.set((imp.angle.get() + speed * dt) % 360.0);
        if imp.spectrum.get() {
            let frame = playback.playing().then(|| playback.spectrum()).flatten();
            let mut normaliser = imp.normaliser.borrow_mut();
            let targets = match frame {
                Some(frame) => normaliser.update(&frame, dt),
                None => {
                    // Paused or stopped: relearn the music's usual level on resuming.
                    normaliser.reset();
                    [0.0; POINTS]
                }
            };
            drop(normaliser);
            let mut levels = imp.levels.borrow_mut();
            let before = *levels;
            spectrum_levels::smooth(
                &mut levels,
                &targets,
                dt,
                SPECTRUM_ATTACK_S,
                SPECTRUM_RELEASE_S,
            );
            moving |= levels
                .iter()
                .zip(before)
                .any(|(now, then)| (now - then).abs() > 1e-4);
        }
        if moving {
            self.queue_draw();
        }
    }
}

/// Draws `paintable` filling a `size` square, cropped to the middle if it isn't square.
fn draw_cover(snapshot: &gtk::Snapshot, paintable: &gdk::Paintable, size: f32) {
    let aspect = paintable.intrinsic_aspect_ratio();
    let (w, h) = match aspect {
        a if a > 1.0 => (size * a as f32, size),
        a if a > 0.0 => (size, size / a as f32),
        _ => (size, size),
    };
    snapshot.save();
    snapshot.translate(&graphene::Point::new((size - w) / 2.0, (size - h) / 2.0));
    paintable.snapshot(snapshot, f64::from(w), f64::from(h));
    snapshot.restore();
}

/// Points around the spectrum's outline, centred on `centre`: `rest` from the centre in
/// silence, up to `reach` further out at full level. Pitch runs evenly from bass at the
/// left and right to treble at the top and bottom, mirrored both ways; the outline is
/// then smoothed around the circle so it stays round, never square.
fn spectrum_outline(
    levels: &Levels,
    (cx, cy): (f32, f32),
    rest: f32,
    reach: f32,
) -> Vec<(f32, f32)> {
    use std::f32::consts::{FRAC_PI_2, TAU};
    let angle = |i: usize| TAU * i as f32 / SPECTRUM_SAMPLES as f32;
    let mut extra: Vec<f32> = (0..SPECTRUM_SAMPLES)
        .map(|i| {
            // Angle from the horizontal, 0 (sides) to 1 (top and bottom).
            let u = angle(i).sin().abs().asin() / FRAC_PI_2;
            reach * level_at(levels, u)
        })
        .collect();
    for _ in 0..OUTLINE_BLUR_PASSES {
        let prev = extra.clone();
        let n = prev.len();
        for (i, e) in extra.iter_mut().enumerate() {
            *e = (prev[(i + n - 1) % n] + 2.0 * prev[i] + prev[(i + 1) % n]) / 4.0;
        }
    }
    extra
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let (sin, cos) = angle(i).sin_cos();
            let r = rest + e;
            (cx + r * cos, cy + r * sin)
        })
        .collect()
}

/// The cover's colour made bright enough to glow: lightness and saturation raised.
fn glow_colour((r, g, b): (u8, u8, u8)) -> gdk::RGBA {
    let (r, g, b) = (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let lightness = (max + min) / 2.0;
    let chroma = max - min;
    let hue = if chroma == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / chroma + 2.0)
    } else {
        60.0 * ((r - g) / chroma + 4.0)
    };
    let saturation = if chroma == 0.0 {
        0.0
    } else {
        (chroma / (1.0 - (2.0 * lightness - 1.0).abs())).max(0.6)
    };
    let lightness = lightness.clamp(0.55, 0.7);
    // Back from HSL.
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation.min(1.0);
    let x = c * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hue as u32 / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = lightness - c / 2.0;
    gdk::RGBA::new(r + m, g + m, b + m, 0.9)
}

/// The disc's next speed: eased towards `target`, so it never jumps; a disc that has
/// all but stopped stops.
fn ease_speed(speed: f64, target: f64, dt: f64) -> f64 {
    let next = speed + (target - speed) * (1.0 - (-dt / EASE_S).exp());
    if target == 0.0 && next.abs() < STILL_DEG_S {
        0.0
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1.0 / 60.0;
    const FULL: f64 = 360.0 / TURN_S;

    fn run(mut speed: f64, target: f64, seconds: f64) -> (f64, f64) {
        let mut biggest_step = 0.0_f64;
        for _ in 0..(seconds / FRAME) as usize {
            let next = ease_speed(speed, target, FRAME);
            biggest_step = biggest_step.max((next - speed).abs());
            speed = next;
        }
        (speed, biggest_step)
    }

    #[test]
    fn spins_up_gradually() {
        let (after_one_frame, _) = run(0.0, FULL, FRAME);
        assert!(after_one_frame > 0.0 && after_one_frame < FULL * 0.05);
        let (speed, step) = run(0.0, FULL, 4.0);
        assert!((speed - FULL).abs() < FULL * 0.01, "{speed}");
        // No frame changes the speed by more than a few percent of full speed.
        assert!(step < FULL * 0.05, "step {step}");
    }

    #[test]
    fn coasts_to_a_stop() {
        let (after_half_a_second, _) = run(FULL, 0.0, 0.5);
        assert!(after_half_a_second > FULL * 0.3, "{after_half_a_second}");
        let (speed, step) = run(FULL, 0.0, 10.0);
        assert_eq!(speed, 0.0, "comes to rest");
        assert!(step < FULL * 0.05, "step {step}");
    }

    #[test]
    fn spectrum_is_round_in_silence_and_mirrored_both_ways() {
        let quiet = spectrum_outline(&[0.0; POINTS], (100.0, 100.0), 50.0, 30.0);
        for (x, y) in &quiet {
            assert!(((x - 100.0).hypot(y - 100.0) - 50.0).abs() < 1e-3);
        }
        let mut levels = [0.0; POINTS];
        for (i, l) in levels.iter_mut().enumerate() {
            *l = 1.0 - i as f32 / POINTS as f32;
        }
        let pts = spectrum_outline(&levels, (0.0, 0.0), 50.0, 30.0);
        let radius = |i: usize| {
            pts[i % SPECTRUM_SAMPLES]
                .0
                .hypot(pts[i % SPECTRUM_SAMPLES].1)
        };
        let n = SPECTRUM_SAMPLES;
        // Bass (loudest here) at the sides, treble at the top and bottom.
        assert!(radius(0) > radius(n / 4) && radius(n / 2) > radius(3 * n / 4));
        // Mirrored left-right and top-bottom.
        for i in 0..n {
            assert!((radius(i) - radius(n - i)).abs() < 1e-3);
            assert!((radius(i) - radius(n / 2 + i)).abs() < 1e-3);
        }
    }

    #[test]
    fn one_loud_pitch_makes_a_smooth_peak_and_leaves_the_rest_still() {
        // One loud pitch as the normaliser hands it over: tapering off either side.
        let mut levels = [0.0; POINTS];
        for (i, level) in levels.iter_mut().enumerate() {
            *level = 1.5_f32.powi(-(i.abs_diff(POINTS / 2) as i32));
        }
        let (rest, reach) = (50.0, 30.0);
        let pts = spectrum_outline(&levels, (0.0, 0.0), rest, reach);
        let radii: Vec<f32> = pts.iter().map(|(x, y)| x.hypot(*y)).collect();
        let n = radii.len();
        // Far from the peak (at the sides, where bass sits) the outline stays at rest.
        assert!((radii[0] - rest).abs() < 0.5, "side radius {}", radii[0]);
        // The peak reaches out most of the way...
        let highest = radii.iter().copied().fold(0.0, f32::max);
        assert!(highest > rest + reach * 0.6, "peak {highest}");
        // ...without steep steps or a flat top.
        let steepest = (0..n)
            .map(|i| (radii[(i + 1) % n] - radii[i]).abs())
            .fold(0.0, f32::max);
        assert!(steepest < reach * 0.15, "step {steepest}");
        let near_top = rest + 0.9 * (highest - rest);
        let flat = (0..n)
            .filter(|&i| radii[i] > near_top && (radii[(i + 1) % n] - radii[i]).abs() < 1e-3)
            .count();
        assert!(flat <= 4, "{flat} flat points near the peaks");
    }

    #[test]
    fn glow_colour_is_bright() {
        for colour in [(0, 0, 0), (20, 40, 30), (255, 255, 255), (200, 30, 30)] {
            let c = glow_colour(colour);
            let lightness =
                (c.red().max(c.green()).max(c.blue()) + c.red().min(c.green()).min(c.blue())) / 2.0;
            assert!((0.54..=0.71).contains(&lightness), "{colour:?} -> {c:?}");
        }
        // Keeps the hue: red stays red.
        let red = glow_colour((200, 30, 30));
        assert!(red.red() > red.green() && red.red() > red.blue());
    }

    #[test]
    fn reversing_mid_way_has_no_jump() {
        let (slowing, _) = run(FULL, 0.0, 0.3);
        let next = ease_speed(slowing, FULL, FRAME);
        assert!(next > slowing && next - slowing < FULL * 0.05);
    }
}
