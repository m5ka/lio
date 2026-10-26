//! A "Download" toggle button for a pin (playlist, Favourites), kept in sync with the
//! downloads worker.

use gettextrs::gettext;
use gtk::glib;
use gtk::prelude::*;
use lio_store::downloads::PinKind;

use crate::application::LioApplication;

pub fn new(kind: PinKind, action_kind: &'static str, id: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::builder()
        .icon_name("folder-download-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["circular", "flat"])
        .build();
    let updating = std::rc::Rc::new(std::cell::Cell::new(false));
    let id = id.to_owned();
    let update = glib::clone!(
        #[weak]
        button,
        #[strong]
        updating,
        #[strong]
        id,
        move || {
            let app = LioApplication::default();
            let pin = app.offline().pin_state(kind, &id);
            updating.set(true);
            button.set_active(pin.is_some());
            updating.set(false);
            let label = match &pin {
                Some(p) if p.available_offline() => gettext("Available Offline — Remove Download"),
                Some(_) => gettext("Downloading — Cancel"),
                None => gettext("Download"),
            };
            button.set_tooltip_text(Some(&label));
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
            button.set_visible(app.session().account().is_some_and(|a| a.can_download));
        }
    );
    update();
    let handler = LioApplication::default()
        .offline()
        .connect_downloads_changed({
            let update = update.clone();
            move |_, _| update()
        });
    let handler = std::cell::Cell::new(Some(handler));
    button.connect_destroy(move |_| {
        if let Some(id) = handler.take() {
            LioApplication::default().offline().disconnect(id);
        }
    });
    button.connect_toggled(move |b| {
        if updating.get() {
            return;
        }
        if let Some(win) = LioApplication::default().window() {
            let _ = gtk::prelude::WidgetExt::activate_action(
                &win,
                "item.download",
                Some(&(action_kind, id.as_str()).to_variant()),
            );
        }
        let _ = b;
    });
    button
}
