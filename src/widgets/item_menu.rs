//! Context menu for albums, artists, tracks and playlists.
//!
//! Opened by right-click or long-press. Entries activate `item.*` actions (see
//! `actions::items`) with a `(ss)` parameter `(kind, id)`.

use gettextrs::gettext;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// What a menu acts on. IDs are server IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Album(String, Option<String>),
    Artist(String),
    Track {
        id: String,
        album_id: Option<String>,
    },
    Playlist(String),
}

impl Target {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Album(..) => "album",
            Self::Artist(_) => "artist",
            Self::Track { .. } => "track",
            Self::Playlist(_) => "playlist",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Album(id, _) | Self::Artist(id) | Self::Playlist(id) => id,
            Self::Track { id, .. } => id,
        }
    }
}

fn entry(label: &str, action: &str, kind: &str, id: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&(kind, id).to_variant()));
    item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
    item
}

pub fn menu_for(target: &Target) -> gio::Menu {
    let (kind, id) = (target.kind(), target.id());
    let menu = gio::Menu::new();

    let play = gio::Menu::new();
    play.append_item(&entry(&gettext("_Play"), "item.play", kind, id));
    play.append_item(&entry(&gettext("Play _Next"), "item.play-next", kind, id));
    play.append_item(&entry(&gettext("_Add to Queue"), "item.enqueue", kind, id));
    menu.append_section(None, &play);

    let library = gio::Menu::new();
    if kind != "playlist" {
        library.append_item(&entry(
            &gettext("Add to _Playlist…"),
            "item.add-to-playlist",
            kind,
            id,
        ));
    }
    if kind != "playlist" {
        let label = if crate::actions::annotation::is_starred(kind, id) {
            gettext("Un_star")
        } else {
            gettext("_Star")
        };
        library.append_item(&entry(&label, "item.star", kind, id));
    }
    if kind == "track" {
        let current = crate::actions::annotation::rating(id).unwrap_or(0);
        let rating = gio::Menu::new();
        for n in 1..=5 {
            let label = format!("{}{}", "★".repeat(n), "☆".repeat(5 - n));
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(Some("item.rate"), Some(&(id, n as i32).to_variant()));
            rating.append_item(&item);
        }
        if current > 0 {
            let clear = gio::MenuItem::new(Some(&gettext("_Clear Rating")), None);
            clear.set_action_and_target_value(Some("item.rate"), Some(&(id, 0i32).to_variant()));
            rating.append_item(&clear);
        }
        library.append_submenu(Some(&gettext("_Rating")), &rating);
    }
    if matches!(kind, "album" | "playlist") {
        let pin_kind = if kind == "album" {
            lio_store::downloads::PinKind::Album
        } else {
            lio_store::downloads::PinKind::Playlist
        };
        let pinned = crate::application::LioApplication::default()
            .offline()
            .pin_state(pin_kind, id)
            .is_some();
        let label = if pinned {
            gettext("Remove _Download")
        } else {
            gettext("_Download")
        };
        library.append_item(&entry(&label, "item.download", kind, id));
    }
    menu.append_section(None, &library);

    let go = gio::Menu::new();
    match target {
        Target::Album(_, Some(artist)) => {
            go.append_item(&entry(
                &gettext("Go to _Artist"),
                "item.go-artist",
                "artist",
                artist,
            ));
        }
        Target::Track { album_id, .. } => {
            if let Some(album) = album_id {
                go.append_item(&entry(
                    &gettext("Go to A_lbum"),
                    "item.go-album",
                    "album",
                    album,
                ));
            }
            go.append_item(&entry(
                &gettext("Go to _Artist"),
                "item.go-artist",
                "track",
                id,
            ));
        }
        _ => {}
    }
    if go.n_items() > 0 {
        menu.append_section(None, &go);
    }
    menu
}

/// Shows the menu for `target` at `(x, y)` in `widget` coordinates.
pub fn popup(widget: &impl IsA<gtk::Widget>, target: &Target, x: f64, y: f64) {
    let popover = gtk::PopoverMenu::from_model(Some(&menu_for(target)));
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_parent(widget);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    popover.popup();
}

/// Opens the menu on right-click, long-press and the Menu key. `target` is evaluated
/// when the menu opens (rows are recycled).
pub fn attach<W: IsA<gtk::Widget>>(widget: &W, target: impl Fn(&W) -> Option<Target> + 'static) {
    let target = std::rc::Rc::new(target);

    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    let t = target.clone();
    click.connect_pressed(move |gesture, _, x, y| {
        let Some(widget) = gesture.widget().and_downcast::<W>() else {
            return;
        };
        if let Some(target) = t(&widget) {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            popup(&widget, &target, x, y);
        }
    });
    widget.add_controller(click);

    let long = gtk::GestureLongPress::builder().touch_only(true).build();
    let t = target.clone();
    long.connect_pressed(move |gesture, x, y| {
        let Some(widget) = gesture.widget().and_downcast::<W>() else {
            return;
        };
        if let Some(target) = t(&widget) {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            popup(&widget, &target, x, y);
        }
    });
    widget.add_controller(long);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |ctrl, key, _, mods| {
        let is_menu_key = key == gdk::Key::Menu
            || (key == gdk::Key::F10 && mods.contains(gdk::ModifierType::SHIFT_MASK));
        let Some(widget) = ctrl.widget().and_downcast::<W>() else {
            return glib::Propagation::Proceed;
        };
        if is_menu_key && let Some(target) = target(&widget) {
            popup(
                &widget,
                &target,
                widget.width() as f64 / 2.0,
                widget.height() as f64 / 2.0,
            );
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    widget.add_controller(keys);
}
