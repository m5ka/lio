//! Track-change notifications: off by default, only when the window isn't active.

use gettextrs::gettext;
use gtk::prelude::*;
use gtk::{gio, glib};

use crate::application::LioApplication;
use crate::covers::SIZE_LARGE;
use crate::playback_controller::PlaybackController;

const ID: &str = "now-playing";

pub fn setup(app: &LioApplication, controller: &PlaybackController) {
    controller.connect_track_started(glib::clone!(
        #[weak]
        app,
        move |c| {
            if !app.settings().notifications() || app.window().is_some_and(|w| w.is_active()) {
                return;
            }
            let Some(track) = c.current_track() else {
                return;
            };
            let notification = gio::Notification::new(track.title());
            let body = match track.album() {
                Some(album) if !album.is_empty() => format!("{} — {album}", track.artist()),
                _ => track.artist(),
            };
            notification.set_body(Some(&body));
            notification.set_category(Some("x-gnome.music"));
            notification.add_button(&gettext("Next"), "app.next");
            if let (Some(cover), Some(covers)) = (track.cover_art_id(), app.covers()) {
                let path = covers.path(cover, SIZE_LARGE);
                if path.exists() {
                    notification.set_icon(&gio::FileIcon::new(&gio::File::for_path(path)));
                }
            }
            app.send_notification(Some(ID), &notification);
        }
    ));
}
