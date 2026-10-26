//! Shared helper: show a cover in a `(Gtk.Stack, Gtk.Picture)` pair with a placeholder.

use std::cell::RefCell;

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::application::LioApplication;

/// Loads `cover_id` at `size` into `picture` (a `Gtk.Picture` or `Gtk.Image`), showing `stack`'s "placeholder" page until
/// it arrives. Cancels the previous load tracked in `slot` (row recycling).
pub fn show_cover(
    stack: &gtk::Stack,
    picture: &impl IsA<gtk::Widget>,
    cover_id: Option<&str>,
    size: u32,
    slot: &RefCell<Option<gio::Cancellable>>,
) {
    if let Some(previous) = slot.take() {
        previous.cancel();
    }
    let Some(id) = cover_id.filter(|id| !id.is_empty()) else {
        picture.set_property("paintable", None::<&gtk::gdk::Paintable>);
        stack.set_visible_child_name("placeholder");
        return;
    };
    let Some(covers) = LioApplication::default().covers() else {
        stack.set_visible_child_name("placeholder");
        return;
    };
    if let Some(texture) = covers.cached(id, size) {
        picture.set_property("paintable", &texture);
        stack.set_visible_child_name("cover");
        return;
    }
    picture.set_property("paintable", None::<&gtk::gdk::Paintable>);
    stack.set_visible_child_name("placeholder");
    let cancellable = gio::Cancellable::new();
    slot.replace(Some(cancellable.clone()));
    let id = id.to_owned();
    let picture = picture.clone().upcast::<gtk::Widget>();
    glib::spawn_future_local(glib::clone!(
        #[weak]
        stack,
        #[weak]
        picture,
        async move {
            if let Some(texture) = covers.load(&id, size, Some(&cancellable)).await {
                picture.set_property("paintable", &texture);
                stack.set_visible_child_name("cover");
            }
        }
    ));
}
