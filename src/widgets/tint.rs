//! Colour washes taken from cover art (Now Playing, album pages).

use std::sync::atomic::{AtomicU32, Ordering};

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::application::LioApplication;

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
