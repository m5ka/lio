//! Library sync: mirrors the server into SQLite.
//!
//! - First sync: all artists, then every album page (`alphabeticalByName`, 500 per page).
//! - Later syncs: skipped when `getScanStatus.lastScan` is unchanged; otherwise new albums
//!   are paged from `newest` until a known ID appears. Every 24 h a full album-ID diff
//!   removes albums deleted on the server.
//! - Playlists and favourites are refreshed every time (cheap).
//! - Album track lists are fetched lazily (album page, downloads) and by [`sync_tracks`]
//!   in the background, so offline search covers tracks too.

use std::collections::HashSet;

use lio_subsonic::{AlbumListType, Client};

use crate::db::{Db, now};
use crate::error::Result;
use crate::library;

/// How often the full album-ID diff runs.
pub const ALBUM_DIFF_INTERVAL: i64 = 24 * 3600;
const KEY_LAST_SCAN: &str = "last_scan";
const KEY_FULL_SYNC: &str = "full_sync_completed_at";
const KEY_ALBUM_DIFF: &str = "last_album_diff_at";
const KEY_IGNORED_ARTICLES: &str = "ignored_articles";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub full: bool,
    pub albums_upserted: usize,
    pub albums_removed: usize,
    pub artists: usize,
    pub playlists: usize,
    /// Nothing changed on the server since the last sync.
    pub unchanged: bool,
}

/// Runs one sync. `force_full` re-lists every album (e.g. "Refresh Library").
pub async fn sync(
    client: &Client,
    db: &Db,
    username: &str,
    force_full: bool,
) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    let scan = client.get_scan_status().await.ok();
    let last_scan = scan
        .as_ref()
        .and_then(|s| s.last_scan.clone())
        .unwrap_or_default();
    let (stored_scan, full_done, last_diff) = db
        .run(|c| {
            Ok((
                library::sync_value(c, KEY_LAST_SCAN)?,
                library::sync_value(c, KEY_FULL_SYNC)?,
                library::sync_value(c, KEY_ALBUM_DIFF)?.and_then(|v| v.parse::<i64>().ok()),
            ))
        })
        .await?;

    let needs_full = force_full
        || full_done.is_none()
        || last_diff.is_none_or(|t| now() - t >= ALBUM_DIFF_INTERVAL);
    let library_changed =
        stored_scan.as_deref() != Some(last_scan.as_str()) || last_scan.is_empty();

    if needs_full || library_changed {
        sync_artists(client, db, &mut report).await?;
        if needs_full {
            full_album_sync(client, db, &mut report).await?;
        } else {
            incremental_album_sync(client, db, &mut report).await?;
        }
    } else {
        report.unchanged = true;
    }

    sync_playlists(client, db, username, &mut report).await?;
    let starred = client.get_starred2().await?;
    db.run(move |c| library::apply_starred(c, &starred)).await?;

    let scan_value = last_scan.clone();
    db.run(move |c| {
        library::set_sync_value(c, KEY_LAST_SCAN, &scan_value)?;
        library::prune_fts(c)
    })
    .await?;
    tracing::info!(?report, "library sync finished");
    Ok(report)
}

async fn sync_artists(client: &Client, db: &Db, report: &mut SyncReport) -> Result<()> {
    let index = client.get_artists().await?;
    report.artists = index.artists().count();
    db.run(move |c| {
        let tx = c.transaction()?;
        let mut seen = Vec::new();
        for artist in index.artists() {
            seen.push(library::upsert_artist(&tx, artist)?);
        }
        library::set_sync_value(&tx, KEY_IGNORED_ARTICLES, &index.ignored_articles)?;
        // Remove artists the server no longer lists and nothing references.
        tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS seen_ids (rowid INTEGER PRIMARY KEY); DELETE FROM seen_ids;")?;
        {
            let mut insert = tx.prepare("INSERT OR IGNORE INTO seen_ids (rowid) VALUES (?1)")?;
            for rowid in seen {
                insert.execute([rowid])?;
            }
        }
        tx.execute(
            "DELETE FROM artist WHERE rowid NOT IN (SELECT rowid FROM seen_ids)
               AND rowid NOT IN (SELECT artist_rowid FROM album_artist)
               AND rowid NOT IN (SELECT artist_rowid FROM track_artist)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    })
    .await
}

async fn full_album_sync(client: &Client, db: &Db, report: &mut SyncReport) -> Result<()> {
    report.full = true;
    let albums = client
        .get_all_albums(&AlbumListType::AlphabeticalByName)
        .await?;
    report.albums_upserted = albums.len();
    let ids: HashSet<String> = albums.iter().map(|a| a.id.clone()).collect();
    for chunk in albums.chunks(500) {
        let chunk = chunk.to_vec();
        db.run(move |c| library::upsert_albums(c, &chunk).map(drop))
            .await?;
    }
    report.albums_removed = db
        .run(move |c| {
            let tx = c.transaction()?;
            let existing: Vec<(i64, String)> = {
                let mut stmt = tx.prepare("SELECT rowid, id FROM album")?;
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?
            };
            let mut removed = 0;
            for (rowid, id) in existing {
                if !ids.contains(&id) {
                    tx.execute("DELETE FROM album WHERE rowid = ?1", [rowid])?;
                    removed += 1;
                }
            }
            let t = now().to_string();
            library::set_sync_value(&tx, KEY_FULL_SYNC, &t)?;
            library::set_sync_value(&tx, KEY_ALBUM_DIFF, &t)?;
            tx.commit()?;
            Ok(removed)
        })
        .await?;
    Ok(())
}

async fn incremental_album_sync(client: &Client, db: &Db, report: &mut SyncReport) -> Result<()> {
    let mut offset = 0;
    loop {
        let page = client
            .get_album_list2(
                &AlbumListType::Newest,
                lio_subsonic::ALBUM_PAGE_SIZE,
                offset,
            )
            .await?;
        let n = page.len() as u32;
        let ids: Vec<String> = page.iter().map(|a| a.id.clone()).collect();
        let known = db
            .run(move |c| {
                let mut stmt = c.prepare_cached("SELECT 1 FROM album WHERE id = ?1")?;
                let mut known = 0;
                for id in &ids {
                    if stmt.exists([id])? {
                        known += 1;
                    }
                }
                Ok(known)
            })
            .await?;
        report.albums_upserted += page.len();
        db.run(move |c| library::upsert_albums(c, &page).map(drop))
            .await?;
        // Stop once a page is all known albums, or the list ends.
        if known == n as usize || n < lio_subsonic::ALBUM_PAGE_SIZE {
            return Ok(());
        }
        offset += n;
    }
}

async fn sync_playlists(
    client: &Client,
    db: &Db,
    username: &str,
    report: &mut SyncReport,
) -> Result<()> {
    let playlists = client.get_playlists().await?;
    report.playlists = playlists.len();
    let user = username.to_owned();
    let list = playlists.clone();
    // Playlists whose content changed since we last fetched their entries.
    let stale: Vec<String> = db
        .run(move |c| {
            let tx = c.transaction()?;
            let mut stale = Vec::new();
            let ids: HashSet<String> = list.iter().map(|p| p.id.clone()).collect();
            for playlist in &list {
                let previous: Option<(i64, Option<i64>)> = tx
                    .query_row(
                        "SELECT changed_at, tracks_synced_at FROM playlist WHERE id = ?1",
                        [&playlist.id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .ok();
                library::upsert_playlist(&tx, playlist, &user)?;
                let changed = playlist.changed_at().unwrap_or(0);
                if previous.is_none_or(|(old, synced)| old != changed || synced.is_none()) {
                    stale.push(playlist.id.clone());
                }
            }
            let existing: Vec<String> = {
                let mut stmt = tx.prepare("SELECT id FROM playlist")?;
                stmt.query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?
            };
            for id in existing.into_iter().filter(|id| !ids.contains(id)) {
                tx.execute("DELETE FROM playlist WHERE id = ?1", [id])?;
            }
            tx.commit()?;
            Ok(stale)
        })
        .await?;
    for id in stale {
        sync_playlist(client, db, username, &id).await?;
    }
    Ok(())
}

/// Fetches one playlist's entries into the mirror.
pub async fn sync_playlist(client: &Client, db: &Db, username: &str, id: &str) -> Result<i64> {
    let playlist = client.get_playlist(id).await?;
    let user = username.to_owned();
    db.run(move |c| library::replace_playlist_tracks(c, &playlist, &user))
        .await
}

/// Fetches an album's tracks into the mirror; returns the album rowid.
pub async fn sync_album_tracks(client: &Client, db: &Db, album_id: &str) -> Result<i64> {
    let album = client.get_album(album_id).await?;
    db.run(move |c| library::upsert_album_with_tracks(c, &album))
        .await
}

/// Background fill: fetches track lists for up to `limit` albums that don't have them yet.
/// Returns how many albums were synced.
pub async fn sync_tracks(client: &Client, db: &Db, limit: usize) -> Result<usize> {
    let pending: Vec<String> = db
        .run(move |c| {
            let mut stmt =
                c.prepare("SELECT id FROM album WHERE tracks_synced_at IS NULL ORDER BY created_at DESC LIMIT ?1")?;
            let ids = stmt.query_map([limit as i64], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok(ids)
        })
        .await?;
    let mut done = 0;
    for id in pending {
        match sync_album_tracks(client, db, &id).await {
            Ok(_) => done += 1,
            Err(crate::Error::Subsonic(lio_subsonic::Error::NotFound)) => {
                db.run(move |c| {
                    Ok(c.execute("DELETE FROM album WHERE id = ?1", [id])
                        .map(drop)?)
                })
                .await?;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}
