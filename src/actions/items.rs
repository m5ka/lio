//! `item.*` actions: play, queue and navigate
//! albums, artists, tracks and playlists by `(kind, id)`.

use gettextrs::gettext;
use gtk::prelude::*;
use gtk::{gio, glib};
use lio_store::{TrackRow, queries, sync};

use crate::application::LioApplication;
use crate::playback_controller::QueueOrigin;
use crate::runtime;
use crate::widgets::LioWindow;

/// Actions enabled per account (downloads need the server's `downloadRole`).
pub const DEFERRED: [&str; 1] = ["download"];

pub fn install(win: &LioWindow) {
    let group = gio::SimpleActionGroup::new();
    let ty = glib::VariantTy::new("(ss)").unwrap();
    for name in [
        "play",
        "play-next",
        "enqueue",
        "go-album",
        "go-artist",
        "add-to-playlist",
        "star",
        "download",
    ] {
        let action = gio::SimpleAction::new(name, Some(ty));
        let action_name = name.to_owned();
        action.connect_activate(glib::clone!(
            #[weak]
            win,
            move |_, param| {
                let Some((kind, id)) = param.and_then(|p| p.get::<(String, String)>()) else {
                    return;
                };
                let name = action_name.clone();
                glib::spawn_future_local(async move { activate(&win, &name, &kind, &id).await });
            }
        ));
        if DEFERRED.contains(&name) {
            action.set_enabled(false);
        }
        group.add_action(&action);
    }
    // `item.rate`: (track id, rating 0–5).
    let rate = gio::SimpleAction::new("rate", Some(glib::VariantTy::new("(si)").unwrap()));
    rate.connect_activate(glib::clone!(
        #[weak]
        win,
        move |_, param| {
            let Some((id, rating)) = param.and_then(|p| p.get::<(String, i32)>()) else {
                return;
            };
            glib::spawn_future_local(async move {
                super::annotation::rate(&win, &id, rating.clamp(0, 5) as u8).await;
            });
        }
    ));
    group.add_action(&rate);
    win.insert_action_group("item", Some(&group));
    win.set_item_actions(group);
}

/// Enables or disables one `item.*` action (used when later stories switch features on).
pub fn set_enabled(win: &LioWindow, name: &str, enabled: bool) {
    if let Some(action) = win
        .item_actions()
        .and_then(|g| g.lookup_action(name))
        .and_downcast::<gio::SimpleAction>()
    {
        action.set_enabled(enabled);
    }
}

async fn activate(win: &LioWindow, action: &str, kind: &str, id: &str) {
    let app = LioApplication::default();
    match action {
        "play" | "play-next" | "enqueue" => {
            let tracks = match resolve_tracks(kind, id).await {
                Ok(t) if !t.is_empty() => t,
                Ok(_) => return win.toast(&gettext("Nothing to play")),
                Err(e) => return win.toast(&e),
            };
            let playback = app.playback();
            match action {
                "play" => playback.play_tracks(tracks, 0, origin(kind, id)),
                "play-next" => {
                    playback.play_next(tracks);
                    win.toast(&gettext("Playing next"));
                }
                _ => {
                    playback.enqueue(tracks);
                    win.toast(&gettext("Added to queue"));
                }
            }
        }
        "star" => super::annotation::toggle_star(win, kind, id).await,
        "download" => {
            let pin_kind = match kind {
                "album" => lio_store::downloads::PinKind::Album,
                "playlist" => lio_store::downloads::PinKind::Playlist,
                "favourites" => lio_store::downloads::PinKind::Favourites,
                _ => return,
            };
            match app.offline().toggle(pin_kind, id).await {
                Ok(true) => win.toast(&gettext("Downloading for offline listening")),
                Ok(false) => win.toast(&gettext("Download removed")),
                Err(e) => win.toast(&e),
            }
        }
        "add-to-playlist" => crate::widgets::add_to_playlist::choose(win, kind, id).await,
        "go-album" => crate::widgets::navigate::album(win, id).await,
        "go-artist" => {
            let artist_id = if kind == "track" {
                artist_of_track(id).await
            } else {
                Some(id.to_owned())
            };
            if let Some(artist_id) = artist_id {
                crate::widgets::navigate::artist(win, &artist_id).await;
            }
        }
        _ => {}
    }
}

fn origin(kind: &str, id: &str) -> QueueOrigin {
    match kind {
        "album" => QueueOrigin::Album(id.to_owned()),
        "playlist" => QueueOrigin::Playlist(id.to_owned()),
        "artist" => QueueOrigin::Artist(id.to_owned()),
        _ => QueueOrigin::None,
    }
}

async fn artist_of_track(track_id: &str) -> Option<String> {
    let account = LioApplication::default().session().account()?;
    let id = track_id.to_owned();
    account
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT artist.id FROM track_artist JOIN artist ON artist.rowid = track_artist.artist_rowid
                 JOIN track ON track.rowid = track_artist.track_rowid WHERE track.id = ?1 LIMIT 1",
                [&id],
                |r| r.get::<_, String>(0),
            )
            .ok())
        })
        .ok()
        .flatten()
}

/// Tracks for a `(kind, id)`, refreshing from the server first when online.
pub async fn resolve_tracks(kind: &str, id: &str) -> Result<Vec<TrackRow>, String> {
    let app = LioApplication::default();
    let session = app.session();
    let Some(account) = session.account() else {
        return Ok(Vec::new());
    };
    let online = session.is_online();
    let (kind, id) = (kind.to_owned(), id.to_owned());
    runtime::run(async move {
        let (client, db) = (account.client.clone(), account.db.clone());
        let err = |e: lio_store::Error| e.to_string();
        match kind.as_str() {
            "album" => {
                let synced = {
                    let id = id.clone();
                    db.run(move |c| {
                        Ok(queries::album_row_by_id(c, &id)?.and_then(|a| a.tracks_synced_at))
                    })
                    .await
                    .map_err(err)?
                };
                if online && synced.is_none() {
                    sync::sync_album_tracks(&client, &db, &id)
                        .await
                        .map_err(err)?;
                }
                db.run(move |c| match queries::album_row_by_id(c, &id)? {
                    Some(a) => queries::album_track_rows(c, a.rowid),
                    None => Ok(Vec::new()),
                })
                .await
                .map_err(err)
            }
            "artist" => {
                if online && let Ok(artist) = client.get_artist(&id).await {
                    for album in &artist.album {
                        let album_id = album.id.clone();
                        let synced = db
                            .run(move |c| {
                                Ok(queries::album_row_by_id(c, &album_id)?
                                    .and_then(|a| a.tracks_synced_at))
                            })
                            .await
                            .map_err(err)?;
                        if synced.is_none() {
                            sync::sync_album_tracks(&client, &db, &album.id)
                                .await
                                .map_err(err)?;
                        }
                    }
                }
                db.run(move |c| match queries::artist_row_by_id(c, &id)? {
                    Some(a) => queries::artist_track_rows(c, a.rowid),
                    None => Ok(Vec::new()),
                })
                .await
                .map_err(err)
            }
            "playlist" => {
                if online {
                    sync::sync_playlist(&client, &db, &account.username, &id)
                        .await
                        .map_err(err)?;
                }
                db.run(move |c| {
                    let rowid: Option<i64> = c
                        .query_row("SELECT rowid FROM playlist WHERE id = ?1", [&id], |r| {
                            r.get(0)
                        })
                        .ok();
                    match rowid {
                        Some(r) => queries::playlist_track_rows(c, r),
                        None => Ok(Vec::new()),
                    }
                })
                .await
                .map_err(err)
            }
            _ => db
                .run(move |c| Ok(queries::track_row_by_id(c, &id)?.into_iter().collect()))
                .await
                .map_err(err),
        }
    })
    .await
}
