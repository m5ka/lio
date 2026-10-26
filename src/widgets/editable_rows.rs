//! Track rows that can be reordered (drag handle, Alt+↑/↓) and removed: used by the
//! queue sidebar and playlist edit mode.

use std::rc::Rc;

use gettextrs::gettext;
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::models::Track;
use crate::widgets::LioTrackListRow;

/// Callbacks for an editable list: remove at a position, move between positions, length.
#[derive(Clone)]
pub struct EditOps {
    pub remove: Rc<dyn Fn(usize)>,
    pub move_item: Rc<dyn Fn(usize, usize)>,
    pub len: Rc<dyn Fn() -> usize>,
}

fn track_row(item: &gtk::ListItem) -> Option<LioTrackListRow> {
    item.child()
        .and_then(|c| c.first_child())
        .and_then(|c| c.next_sibling())
        .and_downcast::<LioTrackListRow>()
}

pub fn factory(ops: EditOps) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = gtk::Box::builder().spacing(6).build();
        let handle = gtk::Image::builder()
            .icon_name("list-drag-handle-symbolic")
            .tooltip_text(gettext("Drag to reorder"))
            .css_classes(["dim-label"])
            .build();
        let track = LioTrackListRow::new();
        track.set_hexpand(true);
        let remove = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(gettext("Remove"))
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        remove.update_property(&[gtk::accessible::Property::Label(&gettext("Remove"))]);
        handle.update_property(&[gtk::accessible::Property::Label(&gettext(
            "Drag to reorder",
        ))]);
        row.append(&handle);
        row.append(&track);
        row.append(&remove);

        let weak = item.downgrade();
        let o = ops.clone();
        remove.connect_clicked(move |_| {
            if let Some(item) = weak.upgrade() {
                (o.remove)(item.position() as usize);
            }
        });

        let drag = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .build();
        let weak = item.downgrade();
        drag.connect_prepare(move |_, _, _| {
            let pos = weak.upgrade()?.position();
            Some(gdk::ContentProvider::for_value(&pos.to_value()))
        });
        handle.add_controller(drag);

        let drop = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let weak = item.downgrade();
        let o = ops.clone();
        drop.connect_drop(move |_, value, _, _| {
            let (Ok(from), Some(item)) = (value.get::<u32>(), weak.upgrade()) else {
                return false;
            };
            (o.move_item)(from as usize, item.position() as usize);
            true
        });
        row.add_controller(drop);

        let keys = gtk::EventControllerKey::new();
        let weak = item.downgrade();
        let o = ops.clone();
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(item) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let pos = item.position() as usize;
            match key {
                gdk::Key::Up if mods.contains(gdk::ModifierType::ALT_MASK) && pos > 0 => {
                    (o.move_item)(pos, pos - 1)
                }
                gdk::Key::Down
                    if mods.contains(gdk::ModifierType::ALT_MASK) && pos + 1 < (o.len)() =>
                {
                    (o.move_item)(pos, pos + 1)
                }
                gdk::Key::Delete => (o.remove)(pos),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        row.add_controller(keys);
        item.set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let (Some(track), Some(row)) = (item.item().and_downcast::<Track>(), track_row(item)) {
            row.bind(&track);
        }
    });
    factory.connect_unbind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let Some(row) = track_row(item) {
            row.unbind();
        }
    });
    factory
}
