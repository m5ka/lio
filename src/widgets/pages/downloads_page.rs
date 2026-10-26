//! Downloads page: every pinned album, playlist and Favourites, with progress,
//! pause/resume, remove, and "Remove All".

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::glib;
use lio_store::downloads::{Pin, PinKind, Status};
use lio_store::queries;

use crate::application::LioApplication;
use crate::runtime;

fn human_size(bytes: i64) -> String {
    glib::format_size(bytes.max(0) as u64).to_string()
}

fn pin_title(pin: &Pin) -> String {
    let Some(account) = LioApplication::default().session().account() else {
        return pin.ref_id.clone();
    };
    match pin.kind {
        PinKind::Favourites => gettext("Favourite Songs"),
        PinKind::Album => account
            .db
            .read(|c| queries::album_row_by_id(c, &pin.ref_id))
            .ok()
            .flatten()
            .map(|a| a.name)
            .unwrap_or_else(|| gettext("Album")),
        PinKind::Playlist => account
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT name FROM playlist WHERE id = ?1",
                    [&pin.ref_id],
                    |r| r.get::<_, String>(0),
                )
                .ok())
            })
            .ok()
            .flatten()
            .unwrap_or_else(|| gettext("Playlist")),
    }
}

fn status_text(pin: &Pin) -> String {
    let progress = ngettext(
        "{done} of {total} song",
        "{done} of {total} songs",
        pin.total_tracks.max(0) as u32,
    )
    .replace("{done}", &pin.done_tracks.to_string())
    .replace("{total}", &pin.total_tracks.to_string());
    let state = match &pin.status {
        Status::Queued => gettext("Queued"),
        Status::Downloading => gettext("Downloading"),
        Status::Paused => gettext("Paused"),
        Status::Waiting => gettext("Waiting for an allowed network"),
        Status::Complete => gettext("Available offline"),
        Status::Syncing => gettext("Updating"),
        Status::Error(e) if e == lio_store::downloads::DISK_FULL => {
            gettext("Not enough disk space")
        }
        Status::Error(_) => gettext("Failed"),
    };
    format!("{state} · {progress} · {}", human_size(pin.bytes))
}

pub fn page() -> adw::NavigationPage {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let empty = adw::StatusPage::builder()
        .icon_name("folder-download-symbolic")
        .title(gettext("No Downloads"))
        .description(gettext(
            "Download albums and playlists to listen without a connection",
        ))
        .vexpand(true)
        .build();
    let clamp = adw::Clamp::builder()
        .maximum_size(800)
        .valign(gtk::Align::Start)
        .child(&list)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&scrolled, Some("list"));

    let remove_all = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(gettext("Remove All Downloads"))
        .build();
    remove_all.update_property(&[gtk::accessible::Property::Label(&gettext(
        "Remove All Downloads",
    ))]);
    let header = adw::HeaderBar::new();
    header.pack_end(&remove_all);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    let page = adw::NavigationPage::builder()
        .title(gettext("Downloads"))
        .tag("downloads")
        .child(&toolbar)
        .build();

    let reload = glib::clone!(
        #[weak]
        list,
        #[weak]
        stack,
        #[weak]
        remove_all,
        move || {
            let pins = LioApplication::default()
                .session()
                .account()
                .and_then(|a| a.db.read(lio_store::downloads::pins).ok())
                .unwrap_or_default();
            list.remove_all();
            for pin in &pins {
                list.append(&pin_row(pin));
            }
            stack.set_visible_child_name(if pins.is_empty() { "empty" } else { "list" });
            remove_all.set_sensitive(!pins.is_empty());
        }
    );
    reload();
    let offline = LioApplication::default().offline();
    let reload_rc = std::rc::Rc::new(reload);
    let r = reload_rc.clone();
    let handler = std::cell::Cell::new(Some(offline.connect_downloads_changed(move |_, _| r())));
    page.connect_destroy(glib::clone!(
        #[weak]
        offline,
        move |_| {
            if let Some(id) = handler.take() {
                offline.disconnect(id);
            }
        }
    ));

    remove_all.connect_clicked(glib::clone!(
        #[weak]
        page,
        move |_| {
            glib::spawn_future_local(async move {
                let dialog = adw::AlertDialog::builder()
                    .heading(gettext("Remove All Downloads?"))
                    .body(gettext("Downloaded music will be deleted from this device. You can download it again later."))
                    .close_response("cancel")
                    .default_response("cancel")
                    .build();
                dialog.add_responses(&[("cancel", &gettext("_Cancel")), ("remove", &gettext("_Remove All"))]);
                dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                if dialog.choose_future(Some(&page)).await != "remove" {
                    return;
                }
                if let Some(dl) = LioApplication::default().offline().downloader() {
                    let _ = runtime::run(async move { dl.remove_all().await }).await;
                }
            });
        }
    ));
    page
}

fn pin_row(pin: &Pin) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(pin_title(pin))
        .subtitle(status_text(pin))
        .build();
    let icon = match pin.kind {
        PinKind::Album => "media-optical-cd-audio-symbolic",
        PinKind::Playlist => "view-list-symbolic",
        PinKind::Favourites => "starred-symbolic",
    };
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    if pin.total_tracks > 0 && pin.status != Status::Complete {
        let bar = gtk::ProgressBar::builder()
            .fraction(pin.done_tracks as f64 / pin.total_tracks as f64)
            .valign(gtk::Align::Center)
            .width_request(80)
            .build();
        row.add_suffix(&bar);
    }
    let rowid = pin.rowid;
    let (icon, label, resume) = match pin.status {
        Status::Paused | Status::Error(_) => {
            ("media-playback-start-symbolic", gettext("Resume"), true)
        }
        Status::Complete => ("", String::new(), false),
        _ => ("media-playback-pause-symbolic", gettext("Pause"), false),
    };
    if !icon.is_empty() {
        let button = gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(&label)
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        button.update_property(&[gtk::accessible::Property::Label(&label)]);
        button.connect_clicked(move |_| {
            let Some(dl) = LioApplication::default().offline().downloader() else {
                return;
            };
            runtime::spawn(async move {
                let _ = if resume {
                    dl.resume(rowid).await
                } else {
                    dl.pause(rowid).await
                };
            });
        });
        row.add_suffix(&button);
    }
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(gettext("Remove Download"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    remove.update_property(&[gtk::accessible::Property::Label(&gettext(
        "Remove Download",
    ))]);
    remove.connect_clicked(move |_| {
        let Some(dl) = LioApplication::default().offline().downloader() else {
            return;
        };
        runtime::spawn(async move {
            let _ = dl.remove(rowid).await;
        });
    });
    row.add_suffix(&remove);
    row
}
