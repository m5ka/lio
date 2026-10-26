//! Artists, genres and playlists sections, built on [`LioListPage`].

use gettextrs::{gettext, ngettext};
use gtk::prelude::*;
use gtk::{gio, glib};
use lio_store::queries::{self, AlbumFilter, AlbumSort};

use crate::application::LioApplication;
use crate::models::{Artist, IdListModel, ItemKind, Playlist};
use crate::widgets::{LioAlbumGrid, LioArtistPage, LioListPage, LioListRow, item_menu};

fn albums_text(n: i64) -> String {
    ngettext("{} album", "{} albums", n.max(0) as u32).replace("{}", &n.to_string())
}

/// Reloads `f` whenever the library changes, for as long as `page` lives.
fn on_library_changed(page: &LioListPage, f: impl Fn() + 'static) {
    let library = LioApplication::default().library();
    let id = std::cell::Cell::new(Some(library.connect_changed(move |_| f())));
    page.connect_destroy(glib::clone!(
        #[weak]
        library,
        move |_| {
            if let Some(id) = id.take() {
                library.disconnect(id);
            }
        }
    ));
}

pub fn artists() -> LioListPage {
    let page = LioListPage::new(&gettext("Artists"), "artists");
    page.set_empty("system-users-symbolic", &gettext("No Artists"));
    let Some(account) = LioApplication::default().session().account() else {
        return page;
    };
    let model = IdListModel::new(ItemKind::Artist, &account.db);
    let reload = glib::clone!(
        #[weak]
        model,
        #[strong(rename_to = db)]
        account.db,
        move || model.set_ids(
            db.read(|c| queries::artist_ids(c, false))
                .unwrap_or_default()
        )
    );
    reload();
    on_library_changed(&page, reload);
    page.setup(
        &model,
        |row, obj| {
            let Some(artist) = obj.downcast_ref::<Artist>() else {
                return;
            };
            row.set_text(artist.name(), &albums_text(artist.row().album_count));
            row.set_cover(artist.cover_art_id());
            row.set_data(artist);
            attach_menu(row);
        },
        |obj| {
            if let (Some(artist), Some(win)) = (
                obj.downcast_ref::<Artist>(),
                LioApplication::default().window(),
            ) {
                win.push_page(&LioArtistPage::new(artist));
            }
        },
    );
    page
}

pub fn genres() -> LioListPage {
    let page = LioListPage::new(&gettext("Genres"), "genres");
    page.set_empty("tag-symbolic", &gettext("No Genres"));
    let Some(account) = LioApplication::default().session().account() else {
        return page;
    };
    let store = gio::ListStore::new::<gtk::StringObject>();
    let counts = std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::<
        String,
        i64,
    >::new()));
    let reload = glib::clone!(
        #[weak]
        store,
        #[strong]
        counts,
        #[strong(rename_to = db)]
        account.db,
        move || {
            let genres = db.read(queries::genres).unwrap_or_default();
            let items: Vec<gtk::StringObject> = genres
                .iter()
                .map(|(g, _)| gtk::StringObject::new(g))
                .collect();
            *counts.borrow_mut() = genres.into_iter().collect();
            store.splice(0, store.n_items(), &items);
        }
    );
    reload();
    on_library_changed(&page, reload);
    page.setup(
        &store,
        move |row, obj| {
            let Some(genre) = obj.downcast_ref::<gtk::StringObject>() else {
                return;
            };
            let name = genre.string();
            let n = counts.borrow().get(name.as_str()).copied().unwrap_or(0);
            row.set_text(&name, &albums_text(n));
            row.set_icon("tag-symbolic");
        },
        |obj| {
            let (Some(genre), Some(win)) = (
                obj.downcast_ref::<gtk::StringObject>(),
                LioApplication::default().window(),
            ) else {
                return;
            };
            let name = genre.string().to_string();
            let grid = LioAlbumGrid::new(&name, AlbumFilter::Genre(name.clone()), AlbumSort::Name);
            win.push_page(&grid);
        },
    );
    page
}

/// Context menu for artist rows (attached once per row widget).
fn attach_menu(row: &LioListRow) {
    if row.widget_name() == "menu-attached" {
        return;
    }
    row.set_widget_name("menu-attached");
    item_menu::attach(row, |row: &LioListRow| {
        let artist = row.data()?.downcast::<Artist>().ok()?;
        Some(item_menu::Target::Artist(artist.id().to_owned()))
    });
}

/// Creates an empty playlist and opens it (the sidebar's "New Playlist").
pub async fn new_playlist() {
    let app = LioApplication::default();
    let (Some(win), Some(account)) = (app.window(), app.session().account()) else {
        return;
    };
    let Some(name) =
        crate::widgets::add_to_playlist::ask_name(&win, &gettext("New Playlist"), "").await
    else {
        return;
    };
    let (client, db, user) = (
        account.client.clone(),
        account.db.clone(),
        account.username.clone(),
    );
    let result = crate::runtime::run(async move {
        let created = client
            .create_playlist(&name, &[])
            .await
            .map_err(|e| e.to_string())?;
        let rowid = lio_store::sync::sync_playlist(&client, &db, &user, &created.id)
            .await
            .map_err(|e| e.to_string())?;
        db.run(move |c| queries::playlist_row(c, rowid))
            .await
            .map_err(|e| e.to_string())
    })
    .await;
    match result {
        Ok(Some(row)) => {
            // Adds its sidebar row, then opens it there.
            app.library().emit_changed();
            win.show_playlist(&Playlist::new(row));
        }
        Ok(None) => {}
        Err(e) => win.toast(&e),
    }
}
