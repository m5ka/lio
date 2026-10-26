//! Development helpers, active only in debug builds.
//!
//! `LIO_SCREENSHOT=out.png` renders the main window to a PNG after
//! `LIO_SCREENSHOT_DELAY_MS` (default 2000) and quits. Only the app's own window is captured.

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::application::LioApplication;
use crate::playback_controller::QueueOrigin;
use crate::runtime;
use crate::widgets::LioWindow;

pub fn schedule_screenshot(win: &LioWindow) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Ok(path) = std::env::var("LIO_SCREENSHOT") else {
        return;
    };
    let delay = std::env::var("LIO_SCREENSHOT_DELAY_MS")
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(2000);
    let win = win.downgrade();
    glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
        let Some(win) = win.upgrade() else { return };
        match render(&win) {
            Some(texture) => match texture.save_to_png(&path) {
                Ok(()) => tracing::info!(%path, "saved screenshot"),
                Err(e) => tracing::error!(%path, "screenshot failed: {e}"),
            },
            None => tracing::error!("screenshot failed: nothing rendered"),
        }
        if let Some(app) = win.application() {
            app.quit();
        }
    });
}

fn render(win: &LioWindow) -> Option<gdk::Texture> {
    let paintable = gtk::WidgetPaintable::new(Some(win));
    let (w, h) = (win.width() as f64, win.height() as f64);
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w, h);
    let node = snapshot.to_node()?;
    let renderer = win.native()?.renderer()?;
    Some(renderer.render_texture(&node, None))
}

/// `LIO_TEST_AUTOPLAY_ALBUM=<album id>`: once signed in, fetch that album and play it
/// (with `LIO_TEST_SHUFFLE`, by pressing the album page's Shuffle button).
pub fn maybe_autoplay(app: &LioApplication) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Ok(album_id) = std::env::var("LIO_TEST_AUTOPLAY_ALBUM") else {
        return;
    };
    let Some(account) = app.session().account() else {
        return;
    };
    let app = app.clone();
    glib::spawn_future_local(async move {
        let result = runtime::run(async move {
            let album = account
                .client
                .get_album(&album_id)
                .await
                .map_err(|e| e.to_string())?;
            account
                .db
                .run(move |c| {
                    let rowid = lio_store::library::upsert_album_with_tracks(c, &album)?;
                    let row = lio_store::queries::album_row(c, rowid)?;
                    Ok((row, lio_store::queries::album_track_rows(c, rowid)?))
                })
                .await
                .map_err(|e| e.to_string())
        })
        .await;
        match result {
            Ok((Some(album), tracks)) => {
                let album = crate::models::Album::new(album);
                let page = crate::widgets::LioAlbumPage::new(&album);
                if let Some(win) = app.window() {
                    win.push_page(&page);
                }
                if std::env::var("LIO_TEST_SHUFFLE").is_ok() {
                    // The page's own Shuffle button, once its tracks have loaded.
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(500),
                        move || {
                            let _ = page.activate_action("album.shuffle", None);
                        },
                    );
                } else {
                    app.playback().play_tracks(
                        tracks,
                        0,
                        QueueOrigin::Album(album.id().to_owned()),
                    );
                }
                if std::env::var("LIO_TEST_LYRICS").is_ok() {
                    let _ = app.settings().set_boolean("now-playing-lyrics", true);
                }
                if std::env::var("LIO_TEST_NOW_PLAYING").is_ok()
                    && let Some(win) = app.window()
                {
                    let _ = gtk::prelude::WidgetExt::activate_action(&win, "win.now-playing", None);
                    if std::env::var("LIO_TEST_FULLSCREEN").is_ok() {
                        win.fullscreen();
                    }
                }
                if std::env::var("LIO_TEST_HIDE").is_ok()
                    && let Some(win) = app.window()
                {
                    // Like closing the window while music plays (background playback).
                    glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
                        win.close()
                    });
                }
                if std::env::var("LIO_TEST_SHOW_QUEUE").is_ok()
                    && let Some(win) = app.window()
                {
                    win.queue_split().set_show_sidebar(true);
                }
            }
            Ok((None, _)) => tracing::error!("autoplay: album missing after sync"),
            Err(e) => tracing::error!("autoplay failed: {e}"),
        }
    });
}

/// `LIO_TEST_SECTION=<home|albums|artists|genres|favourites|downloads|search>`, or
/// `playlist:<name>`: shows that section (or sidebar playlist) once signed in.
/// `LIO_TEST_SEARCH=<text>` types into search.
pub fn maybe_show_section(app: &LioApplication) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Ok(name) = std::env::var("LIO_TEST_SECTION") else {
        return;
    };
    let Some(win) = app.window() else { return };
    // Wait for the first sync so pages have content.
    let app = app.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
        if name == "search" {
            win.show_search();
            if let (Ok(query), Some(page)) = (
                std::env::var("LIO_TEST_SEARCH"),
                win.nav_view()
                    .find_page("search")
                    .and_downcast::<crate::widgets::LioSearchPage>(),
            ) {
                page.set_query(&query);
            }
        } else if let Some(section) = crate::widgets::Section::from_name(&name) {
            win.show_section(section);
        } else if let (Some(wanted), Some(account)) =
            (name.strip_prefix("playlist:"), app.session().account())
        {
            let found = account
                .db
                .read(lio_store::queries::playlists)
                .unwrap_or_default()
                .into_iter()
                .find(|p| p.name == wanted);
            if let Some(row) = found {
                win.show_playlist(&crate::models::Playlist::new(row));
            }
        }
        let _ = app;
    });
}

/// `LIO_TEST_ACTION=<app.x|win.x>`: activates an action once signed in (opens dialogs
/// for screenshots).
pub fn maybe_activate_action(app: &LioApplication) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Ok(action) = std::env::var("LIO_TEST_ACTION") else {
        return;
    };
    let Some(win) = app.window() else { return };
    glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
        let _ = gtk::prelude::WidgetExt::activate_action(&win, &action, None);
    });
}

/// `LIO_TEST_SIZE=<width>x<height>`: initial window size (adaptive layout checks).
pub fn apply_test_size(win: &LioWindow) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Some((w, h)) = std::env::var("LIO_TEST_SIZE").ok().and_then(|s| {
        let (w, h) = s.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    }) else {
        return;
    };
    win.unmaximize();
    win.set_default_size(w, h);
}

/// `LIO_TEST_DOWNLOAD_ALBUM=<album id>`: pins that album once signed in.
pub fn maybe_download(app: &LioApplication) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Ok(id) = std::env::var("LIO_TEST_DOWNLOAD_ALBUM") else {
        return;
    };
    let offline = app.offline();
    glib::spawn_future_local(async move {
        match offline
            .toggle(lio_store::downloads::PinKind::Album, &id)
            .await
        {
            Ok(_) => tracing::info!(album = %id, "test download queued"),
            Err(e) => tracing::error!("test download failed: {e}"),
        }
    });
}
