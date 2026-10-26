//! "Add to Playlist…": pick a writable playlist or create a new one.
//!
//! A dialog rather than a popover: the action is usually invoked from a context menu,
//! which has no widget to anchor a popover to.

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::glib;
use lio_store::queries;

use crate::actions::items;
use crate::application::LioApplication;
use crate::runtime;
use crate::widgets::{LioWindow, error_toast};

/// Asks for a new playlist's name; `None` if cancelled.
pub async fn ask_name(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    initial: &str,
) -> Option<String> {
    let entry = gtk::Entry::builder()
        .placeholder_text(gettext("Playlist name"))
        .text(initial)
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .extra_child(&entry)
        .close_response("cancel")
        .default_response("ok")
        .build();
    dialog.add_responses(&[("cancel", &gettext("_Cancel")), ("ok", &gettext("_Save"))]);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("ok", !initial.trim().is_empty());
    entry.connect_changed(glib::clone!(
        #[weak]
        dialog,
        move |e| dialog.set_response_enabled("ok", !e.text().trim().is_empty())
    ));
    (dialog.choose_future(Some(parent)).await == "ok").then(|| entry.text().trim().to_owned())
}

/// Adds the tracks of `(kind, id)` to a playlist the user picks.
pub async fn choose(win: &LioWindow, kind: &str, id: &str) {
    let app = LioApplication::default();
    let Some(account) = app.session().account() else {
        return;
    };
    let playlists: Vec<_> = account
        .db
        .read(queries::playlists)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.readonly)
        .collect();

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let new_row = adw::ButtonRow::builder()
        .title(gettext("_New Playlist…"))
        .use_underline(true)
        .start_icon_name("list-add-symbolic")
        .build();
    list.append(&new_row);
    for p in &playlists {
        let row = adw::ActionRow::builder()
            .title(&p.name)
            .subtitle(
                ngettext("{} song", "{} songs", p.song_count.max(0) as u32)
                    .replace("{}", &p.song_count.to_string()),
            )
            .activatable(true)
            .build();
        row.set_widget_name(&p.id);
        list.append(&row);
    }
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .propagate_natural_height(true)
        .max_content_height(360)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Add to Playlist"))
        .extra_child(&scrolled)
        .close_response("cancel")
        .build();
    dialog.add_response("cancel", &gettext("_Cancel"));

    let (tx, rx) = async_channel::bounded::<Option<String>>(1);
    list.connect_row_activated(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        tx,
        move |_, row| {
            let target = if row.is::<adw::ButtonRow>() {
                None
            } else {
                Some(row.widget_name().to_string())
            };
            let _ = tx.try_send(target);
            dialog.close();
        }
    ));
    new_row.connect_activated(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        tx,
        move |_| {
            let _ = tx.try_send(None);
            dialog.close();
        }
    ));
    let response = dialog.choose_future(Some(win)).await;
    let Ok(choice) = rx.try_recv() else {
        let _ = response;
        return;
    };

    let tracks = match items::resolve_tracks(kind, id).await {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => return,
        Err(e) => return win.toast(&e),
    };
    let ids: Vec<String> = tracks.into_iter().map(|t| t.id).collect();
    let client = account.client.clone();
    let result = match choice {
        Some(playlist_id) => {
            runtime::run(async move { client.update_playlist(&playlist_id, None, &ids, &[]).await })
                .await
        }
        None => {
            let Some(name) = ask_name(win, &gettext("New Playlist"), "").await else {
                return;
            };
            runtime::run(async move { client.create_playlist(&name, &ids).await.map(drop) }).await
        }
    };
    match result {
        Ok(()) => {
            win.toast(&gettext("Added to playlist"));
            app.library().sync(false);
        }
        Err(e) => win.toast(&error_toast::message(&e)),
    }
}
