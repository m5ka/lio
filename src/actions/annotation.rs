//! Favourites and ratings: optimistic local update, then the server;
//! reverted with a toast if the server refuses.

use gettextrs::gettext;
use lio_store::db::now;
use lio_subsonic::StarTarget;
use rusqlite::{OptionalExtension, params};

use crate::application::LioApplication;
use crate::runtime;
use crate::widgets::{LioWindow, error_toast};

fn table(kind: &str) -> Option<&'static str> {
    match kind {
        "track" => Some("track"),
        "album" => Some("album"),
        "artist" => Some("artist"),
        _ => None,
    }
}

/// Whether `(kind, id)` is starred in the local mirror.
pub fn is_starred(kind: &str, id: &str) -> bool {
    let (Some(table), Some(account)) = (table(kind), LioApplication::default().session().account())
    else {
        return false;
    };
    account
        .db
        .read(|c| {
            Ok(c.query_row(
                &format!("SELECT starred_at IS NOT NULL FROM {table} WHERE id = ?1"),
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
        })
        .unwrap_or(false)
}

pub fn rating(track_id: &str) -> Option<i64> {
    let account = LioApplication::default().session().account()?;
    account
        .db
        .read(|c| {
            Ok(
                c.query_row("SELECT rating FROM track WHERE id = ?1", [track_id], |r| {
                    r.get(0)
                })
                .optional()?
                .flatten(),
            )
        })
        .ok()
        .flatten()
}

fn set_local_star(kind: &str, id: &str, starred: bool) {
    let (Some(table), Some(account)) = (table(kind), LioApplication::default().session().account())
    else {
        return;
    };
    let value = starred.then(now);
    let _ = account.db.with(|c| {
        c.execute(
            &format!("UPDATE {table} SET starred_at = ?1 WHERE id = ?2"),
            params![value, id],
        )?;
        Ok(())
    });
}

/// Toggles the star of a track, album or artist.
pub async fn toggle_star(win: &LioWindow, kind: &str, id: &str) {
    let app = LioApplication::default();
    let Some(account) = app.session().account() else {
        return;
    };
    let target = match kind {
        "track" => StarTarget::Song(id.to_owned()),
        "album" => StarTarget::Album(id.to_owned()),
        "artist" => StarTarget::Artist(id.to_owned()),
        _ => return,
    };
    let starred = !is_starred(kind, id);
    set_local_star(kind, id, starred);
    app.library().emit_changed();
    win.toast(&if starred {
        gettext("Added to favourites")
    } else {
        gettext("Removed from favourites")
    });

    let client = account.client.clone();
    let result = runtime::run(async move {
        if starred {
            client.star(&target).await
        } else {
            client.unstar(&target).await
        }
    })
    .await;
    if let Err(e) = result {
        set_local_star(kind, id, !starred);
        app.library().emit_changed();
        win.toast(&error_toast::message(&e));
    }
}

/// Rates a track 1–5, or clears its rating with 0.
pub async fn rate(win: &LioWindow, track_id: &str, rating: u8) {
    let app = LioApplication::default();
    let Some(account) = app.session().account() else {
        return;
    };
    let previous = self::rating(track_id);
    let value = (rating > 0).then_some(i64::from(rating.min(5)));
    let id = track_id.to_owned();
    let _ = account.db.with(|c| {
        Ok(c.execute(
            "UPDATE track SET rating = ?1 WHERE id = ?2",
            params![value, id],
        )
        .map(drop)?)
    });
    app.library().emit_changed();

    let (client, id) = (account.client.clone(), track_id.to_owned());
    let result = runtime::run(async move { client.set_rating(&id, rating).await }).await;
    if let Err(e) = result {
        let id = track_id.to_owned();
        let _ = account.db.with(|c| {
            Ok(c.execute(
                "UPDATE track SET rating = ?1 WHERE id = ?2",
                params![previous, id],
            )
            .map(drop)?)
        });
        app.library().emit_changed();
        win.toast(&error_toast::message(&e));
    }
}
