//! Colour washes taken from cover art (Now Playing, album pages).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::application::LioApplication;
use crate::covers::SIZE_LARGE;

/// A widget's own CSS rule, set at runtime from its cover. Each instance gets a unique
/// class, so several tinted pages (e.g. album pages in the navigation stack) can
/// coexist; the rule is removed when the `Tint` is dropped.
pub struct Tint {
    class: String,
    provider: gtk::CssProvider,
    display: gdk::Display,
}

impl Tint {
    pub fn new(widget: &impl IsA<gtk::Widget>) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let class = format!("tint-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        widget.add_css_class(&class);
        let provider = gtk::CssProvider::new();
        let display = widget.display();
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        Self {
            class,
            provider,
            display,
        }
    }

    /// Replaces the widget's rule with `declarations` (CSS without selector or braces).
    pub fn set(&self, declarations: &str) {
        self.provider
            .load_from_string(&format!(".{} {{ {declarations} }}", self.class));
    }

    /// Calls `apply` with the accent colour of `cover_id` ([`cover_accent`], so it
    /// matches the player bar and the home page), or for a greyscale cover with no
    /// accent, its average colour.
    pub fn from_cover(
        widget: &impl IsA<gtk::Widget>,
        cover_id: Option<&str>,
        apply: impl Fn((u8, u8, u8)) + 'static,
    ) {
        let Some(id) = cover_id.filter(|id| !id.is_empty()).map(str::to_owned) else {
            return;
        };
        let widget = widget.clone().upcast::<gtk::Widget>().downgrade();
        glib::spawn_future_local(async move {
            let colour = match cover_accent(&id).await {
                Some(colour) => Some(colour),
                None => match LioApplication::default().covers() {
                    Some(covers) => covers
                        .load(&id, SIZE_LARGE, None)
                        .await
                        .and_then(|texture| average_colour(&texture)),
                    None => None,
                },
            };
            if let Some(colour) = colour
                && widget.upgrade().is_some()
            {
                apply(colour);
            }
        });
    }
}

impl Drop for Tint {
    fn drop(&mut self) {
        gtk::style_context_remove_provider_for_display(&self.display, &self.provider);
    }
}

/// Average colour of a (downscaled) texture, ignoring near-transparent pixels.
pub fn average_colour(texture: &gdk::Texture) -> Option<(u8, u8, u8)> {
    let (w, h) = (texture.width() as usize, texture.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let step = (w.max(h) / 64).max(1);
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let i = y * stride + x * 4;
            let px = &bytes[i..i + 4];
            if px[3] < 128 {
                continue;
            }
            r += u64::from(px[0]);
            g += u64::from(px[1]);
            b += u64::from(px[2]);
            n += 1;
        }
    }
    (n > 0).then(|| ((r / n) as u8, (g / n) as u8, (b / n) as u8))
}

/// CSS declarations that make `colour` the accent of a widget and its children. The
/// lightness is clamped so white text stays legible on it, and the standalone accent
/// (icons, scales, text) is derived the same way libadwaita derives its own.
pub fn accent_declarations((r, g, b): (u8, u8, u8)) -> String {
    format!(
        "--accent-bg-color: oklab(from rgb({r},{g},{b}) max(min(l, 0.62), 0.45) a b);
         --accent-fg-color: white;
         --accent-color: oklab(from var(--accent-bg-color) var(--standalone-color-oklab));"
    )
}

/// Accent colour of cover `id`. Always taken from the same size and remembered per
/// cover, so the player bar and Now Playing agree: small thumbnails lose too much
/// saturation to downscaling to give the same result as the large cover.
pub async fn cover_accent(id: &str) -> Option<(u8, u8, u8)> {
    type Accents = HashMap<String, Option<(u8, u8, u8)>>;
    thread_local! {
        static ACCENTS: RefCell<Accents> = RefCell::default();
    }
    if let Some(colour) = ACCENTS.with_borrow(|a| a.get(id).copied()) {
        return colour;
    }
    let covers = LioApplication::default().covers()?;
    let texture = covers.load(id, SIZE_LARGE, None).await?;
    let colour = accent_colour(&texture);
    ACCENTS.with_borrow_mut(|a| a.insert(id.to_owned(), colour));
    colour
}

/// The most prominent colour of a texture, or `None` for covers with (next to) no colour.
pub fn accent_colour(texture: &gdk::Texture) -> Option<(u8, u8, u8)> {
    let (w, h) = (texture.width() as usize, texture.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    accent_from_rgba(&bytes, w, h, stride)
}

/// [`accent_colour`] over RGBA8 pixels. Works in OKLab, so chroma and hue follow what
/// the eye sees (sRGB chroma calls dark and muted colours grey): pixels are binned by
/// hue and weighted by chroma, the heaviest bin is averaged together with its
/// neighbours (so a hue split across a bin edge still counts once), and a muted result
/// gets a little more colour so it reads as an accent.
///
/// Backgrounds make better accents than the people in front of them, so pixels count
/// more towards the edges (where backgrounds are) than in the middle, and skin tones
/// count for little: they win only when the cover has no other colour to speak of.
fn accent_from_rgba(bytes: &[u8], w: usize, h: usize, stride: usize) -> Option<(u8, u8, u8)> {
    const BINS: usize = 36;
    /// Below this chroma a pixel counts as grey; its hue is noise.
    const MIN_CHROMA: f32 = 0.03;
    /// Near-black and near-white pixels are skipped for the same reason.
    const LIGHTNESS: std::ops::RangeInclusive<f32> = 0.15..=0.97;
    /// The winning hues must cover this share of the cover, so a few stray coloured
    /// pixels on a greyscale cover don't make an accent.
    const MIN_SHARE: f32 = 0.03;
    /// A pixel at the edge weighs `1 + EDGE_BOOST` times one in the middle.
    const EDGE_BOOST: f32 = 4.0;
    /// Skin: muted (below `SKIN_CHROMA`), not dark (above `SKIN_LIGHTNESS`), orange hue
    /// (in degrees). Such pixels weigh `SKIN_WEIGHT` times as much as others.
    const SKIN_HUE: std::ops::Range<f32> = 30.0..85.0;
    const SKIN_CHROMA: f32 = 0.13;
    const SKIN_LIGHTNESS: f32 = 0.4;
    const SKIN_WEIGHT: f32 = 0.05;
    /// Muted accents are raised towards this chroma, by at most `MAX_BOOST` times.
    const TARGET_CHROMA: f32 = 0.1;
    const MAX_BOOST: f32 = 2.5;

    let step = (w.max(h) / 64).max(1);
    let mut weights = [0f32; BINS];
    let mut counts = [0usize; BINS];
    // How far (0 to 1) a coordinate is from the middle towards the edge.
    let from_middle = |v: usize, len: usize| {
        if len > 1 {
            (2.0 * v as f32 / (len - 1) as f32 - 1.0).abs()
        } else {
            0.0
        }
    };
    // Coloured pixels: hue bin, weight, OKLab.
    let mut coloured = Vec::new();
    let mut total = 0usize;
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let i = y * stride + x * 4;
            let px = &bytes[i..i + 4];
            if px[3] < 128 {
                continue;
            }
            total += 1;
            let lab = oklab_from_srgb([px[0], px[1], px[2]]);
            let chroma = lab[1].hypot(lab[2]);
            if chroma < MIN_CHROMA || !LIGHTNESS.contains(&lab[0]) {
                continue;
            }
            let hue = (lab[2].atan2(lab[1]) / std::f32::consts::TAU).rem_euclid(1.0);
            let bin = ((hue * BINS as f32) as usize).min(BINS - 1);
            let edge = from_middle(x, w).max(from_middle(y, h));
            let mut weight = chroma * (1.0 + EDGE_BOOST * edge * edge);
            if SKIN_HUE.contains(&(hue * 360.0)) && chroma < SKIN_CHROMA && lab[0] > SKIN_LIGHTNESS
            {
                weight *= SKIN_WEIGHT;
            }
            weights[bin] += weight;
            counts[bin] += 1;
            coloured.push((bin, weight, lab));
        }
    }
    let prev = |bin: usize| (bin + BINS - 1) % BINS;
    let next = |bin: usize| (bin + 1) % BINS;
    let window = |bin: usize| weights[bin] + 0.5 * (weights[prev(bin)] + weights[next(bin)]);
    let share = |bin: usize| counts[prev(bin)] + counts[bin] + counts[next(bin)];
    // The heaviest hues that cover enough of the cover: skin, outweighed by a sliver of
    // background, still wins when that sliver is too small to count.
    let mut ranked: Vec<usize> = (0..BINS).filter(|&bin| window(bin) > 0.0).collect();
    ranked.sort_by(|&a, &b| window(b).total_cmp(&window(a)));
    let best = ranked
        .into_iter()
        .find(|&bin| share(bin) as f32 >= total as f32 * MIN_SHARE)?;
    let (mut weight, mut sum) = (0f32, [0f32; 3]);
    for (bin, w, lab) in coloured {
        if bin == best || bin == prev(best) || bin == next(best) {
            weight += w;
            for (s, v) in sum.iter_mut().zip(lab) {
                *s += w * v;
            }
        }
    }
    let [l, a, b] = sum.map(|s| s / weight);
    let chroma = a.hypot(b);
    if chroma < f32::EPSILON {
        return None;
    }
    // Boost, then pull the chroma back in until the colour fits in sRGB.
    let mut scale = chroma.max(TARGET_CHROMA.min(chroma * MAX_BOOST)) / chroma;
    loop {
        let rgb = linear_from_oklab([l, a * scale, b * scale]);
        if rgb.iter().all(|c| (-1e-4..=1.0 + 1e-4).contains(c)) || scale < 0.05 {
            let [r, g, b] = rgb.map(srgb_from_linear);
            return Some((r, g, b));
        }
        scale *= 0.95;
    }
}

fn oklab_from_srgb(rgb: [u8; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(|c| {
        let c = f32::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn linear_from_oklab([l, a, b]: [f32; 3]) -> [f32; 3] {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    [
        4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_,
        -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_,
    ]
}

fn srgb_from_linear(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let c = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (c * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64×64 RGBA buffer: `share` of the rows in `colour`, the rest in `rest`.
    fn cover(colour: [u8; 3], share: f32, rest: [u8; 3]) -> Vec<u8> {
        let rows = (64.0 * share).round() as usize;
        (0..64 * 64)
            .flat_map(|i| {
                let [r, g, b] = if i / 64 < rows { colour } else { rest };
                [r, g, b, 255]
            })
            .collect()
    }

    fn accent(bytes: &[u8]) -> Option<(u8, u8, u8)> {
        accent_from_rgba(bytes, 64, 64, 64 * 4)
    }

    #[test]
    fn vivid_cover_keeps_its_colour() {
        let (r, g, b) = accent(&cover([200, 40, 40], 1.0, [0; 3])).unwrap();
        assert!(r.abs_diff(200) <= 2 && g.abs_diff(40) <= 2 && b.abs_diff(40) <= 2);
    }

    #[test]
    fn greyscale_cover_has_no_accent() {
        assert_eq!(accent(&cover([200; 3], 0.5, [30; 3])), None);
    }

    #[test]
    fn stray_colour_is_not_an_accent() {
        // One row of 64 is under the minimum share.
        assert_eq!(accent(&cover([200, 40, 40], 1.0 / 64.0, [128; 3])), None);
    }

    #[test]
    fn small_colourful_area_counts() {
        // A tenth of the cover in colour on a grey background.
        let (r, g, b) = accent(&cover([40, 90, 200], 0.1, [128; 3])).unwrap();
        assert!(b > r && b > g);
    }

    #[test]
    fn background_beats_skin() {
        // A portrait: skin over most of the cover, blue along the top.
        let (r, g, b) = accent(&cover([60, 90, 170], 0.15, [225, 170, 140])).unwrap();
        assert!(b > r && b > g, "blue, not skin: {r}, {g}, {b}");
    }

    #[test]
    fn skin_alone_is_still_an_accent() {
        // Skin on white, plus a speck of blue too small to count.
        let mut bytes = cover([225, 170, 140], 0.5, [250; 3]);
        bytes[..4].copy_from_slice(&[60, 90, 170, 255]);
        let (r, g, b) = accent(&bytes).unwrap();
        assert!(r > g && g > b, "skin: {r}, {g}, {b}");
    }

    #[test]
    fn muted_cover_gets_a_stronger_accent() {
        // A dusty brown that sRGB chroma would call grey.
        let input = oklab_from_srgb([130, 112, 95]);
        let (r, g, b) = accent(&cover([130, 112, 95], 1.0, [0; 3])).unwrap();
        let output = oklab_from_srgb([r, g, b]);
        assert!(output[1].hypot(output[2]) > input[1].hypot(input[2]) * 1.5);
        assert!(r > g && g > b, "stays brown: {r}, {g}, {b}");
    }
}
