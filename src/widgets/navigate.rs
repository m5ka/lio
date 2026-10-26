//! Navigation helpers: open album and artist pages by server ID.

use gettextrs::gettext;
use lio_store::queries;

use crate::application::LioApplication;
use crate::models::{Album, Artist};
use crate::runtime;
use crate::widgets::{LioAlbumPage, LioArtistPage, LioWindow};

pub async fn album(win: &LioWindow, album_id: &str) {
    let Some(account) = LioApplication::default().session().account() else {
        return;
    };
    let id = album_id.to_owned();
    let mut row = account
        .db
        .read(|c| queries::album_row_by_id(c, &id))
        .ok()
        .flatten();
    if row.is_none() && LioApplication::default().session().is_online() {
        let (client, db) = (account.client.clone(), account.db.clone());
        let id = id.clone();
        row = runtime::run(async move {
            let rowid = lio_store::sync::sync_album_tracks(&client, &db, &id)
                .await
                .ok()?;
            db.run(move |c| queries::album_row(c, rowid))
                .await
                .ok()
                .flatten()
        })
        .await;
    }
    match row {
        Some(row) => win.push_page(&LioAlbumPage::new(&Album::new(row))),
        None => win.toast(&gettext("This album isn't available")),
    }
}

pub async fn artist(win: &LioWindow, artist_id: &str) {
    let Some(account) = LioApplication::default().session().account() else {
        return;
    };
    let id = artist_id.to_owned();
    let row = account
        .db
        .read(|c| queries::artist_row_by_id(c, &id))
        .ok()
        .flatten();
    match row {
        Some(row) => win.push_page(&LioArtistPage::new(&Artist::new(row))),
        None => win.toast(&gettext("This artist isn't available")),
    }
}
