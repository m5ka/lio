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

    /// Loads `cover_id` at `size` and calls `apply` with its average colour.
    pub fn from_cover(
        widget: &impl IsA<gtk::Widget>,
        cover_id: Option<&str>,
        size: u32,
        apply: impl Fn((u8, u8, u8)) + 'static,
    ) {
        let (Some(id), Some(covers)) = (
            cover_id.filter(|id| !id.is_empty()).map(str::to_owned),
            LioApplication::default().covers(),
        ) else {
            return;
        };
        let widget = widget.clone().upcast::<gtk::Widget>().downgrade();
        glib::spawn_future_local(async move {
            if let Some(texture) = covers.load(&id, size, None).await
                && widget.upgrade().is_some()
                && let Some(colour) = average_colour(&texture)
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

/// The most prominent vivid colour of a texture: pixels are binned by hue, weighted by
/// saturation, and the heaviest bin is averaged. `None` for (near) greyscale covers,
/// which have no colour worth accenting with.
pub fn accent_colour(texture: &gdk::Texture) -> Option<(u8, u8, u8)> {
    const BINS: usize = 24;
    let (w, h) = (texture.width() as usize, texture.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let step = (w.max(h) / 64).max(1);
    // Per bin: weight, weighted r, g, b; plus the total pixel count.
    let mut bins = [(0f64, 0f64, 0f64, 0f64); BINS];
    let mut total = 0usize;
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let i = y * stride + x * 4;
            let px = &bytes[i..i + 4];
            if px[3] < 128 {
                continue;
            }
            total += 1;
            let (r, g, b) = (
                f64::from(px[0]) / 255.0,
                f64::from(px[1]) / 255.0,
                f64::from(px[2]) / 255.0,
            );
            let max = r.max(g).max(b);
            let chroma = max - r.min(g).min(b);
            // Skip greys and near-black pixels; their hue is noise.
            if chroma < 0.15 || max < 0.2 {
                continue;
            }
            let hue = if max == r {
                ((g - b) / chroma).rem_euclid(6.0)
            } else if max == g {
                (b - r) / chroma + 2.0
            } else {
                (r - g) / chroma + 4.0
            } / 6.0;
            let bin = &mut bins[((hue * BINS as f64) as usize).min(BINS - 1)];
            let weight = chroma * chroma;
            bin.0 += weight;
            bin.1 += weight * r;
            bin.2 += weight * g;
            bin.3 += weight * b;
        }
    }
    let (weight, r, g, b) = bins.into_iter().max_by(|a, b| a.0.total_cmp(&b.0))?;
    // A few stray coloured pixels on a greyscale cover don't make an accent.
    if total == 0 || weight < total as f64 * 0.01 {
        return None;
    }
    let channel = |c: f64| (c / weight * 255.0).round() as u8;
    Some((channel(r), channel(g), channel(b)))
}
