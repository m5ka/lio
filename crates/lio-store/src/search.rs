//! Search: local FTS5 over the mirror, topped up from the server's `search3`
//! while the background track sync is incomplete.

use lio_subsonic::{Client, SearchLimits};
use rusqlite::Connection;

use crate::db::Db;
use crate::error::Result;
use crate::library;
use crate::model::{AlbumRow, ArtistRow, TrackRow};
use crate::queries;

#[derive(Debug, Default, Clone)]
pub struct SearchResults {
    pub artists: Vec<ArtistRow>,
    pub albums: Vec<AlbumRow>,
    pub tracks: Vec<TrackRow>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.artists.is_empty() && self.albums.is_empty() && self.tracks.is_empty()
    }
}

/// Builds an FTS5 query: every word must match as a prefix.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

fn ids_for(conn: &Connection, kind: &str, query: &str, raw: &str, limit: u32) -> Result<Vec<i64>> {
    // Prefix matches of the whole text first, then FTS rank.
    let mut stmt = conn.prepare_cached(
        "SELECT ref_rowid FROM search_fts
         WHERE search_fts MATCH ?1 AND kind = ?2
         ORDER BY (text LIKE ?3 || '%') DESC, rank
         LIMIT ?4",
    )?;
    let ids = stmt
        .query_map(rusqlite::params![query, kind, raw, limit], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(ids)
}

/// Searches the local mirror only (works offline).
pub fn search_local(conn: &Connection, text: &str, limits: SearchLimits) -> Result<SearchResults> {
    let Some(query) = fts_query(text) else {
        return Ok(SearchResults::default());
    };
    let raw = text.trim();
    let artists = ids_for(conn, "artist", &query, raw, limits.artists)?;
    let albums = ids_for(conn, "album", &query, raw, limits.albums)?;
    let tracks = ids_for(conn, "track", &query, raw, limits.songs)?;
    Ok(SearchResults {
        artists: queries::artist_rows(conn, &artists)?,
        albums: queries::album_rows(conn, &albums)?,
        tracks: queries::track_rows(conn, &tracks)?,
    })
}

/// Searches locally; when `client` is given and some albums still lack their tracks,
/// asks the server too, stores its results in the mirror, and searches again so both
/// sources are merged and de-duplicated.
pub async fn search(db: &Db, client: Option<&Client>, text: &str) -> Result<SearchResults> {
    let limits = SearchLimits::default();
    let owned = text.to_owned();
    let local = db.run(move |c| search_local(c, &owned, limits)).await?;
    let Some(client) = client else {
        return Ok(local);
    };
    if !db.run(|c| queries::tracks_sync_incomplete(c)).await? {
        return Ok(local);
    }
    let remote = match client.search3(text, limits).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("server search failed, using local results: {e}");
            return Ok(local);
        }
    };
    let owned = text.to_owned();
    db.run(move |c| {
        let tx = c.transaction()?;
        for artist in &remote.artist {
            library::upsert_artist(&tx, artist)?;
        }
        for album in &remote.album {
            library::upsert_album(&tx, album)?;
        }
        for song in &remote.song {
            let album_rowid: Option<i64> = match &song.album_id {
                Some(id) => {
                    use rusqlite::OptionalExtension;
                    tx.query_row("SELECT rowid FROM album WHERE id = ?1", [id], |r| r.get(0))
                        .optional()?
                }
                None => None,
            };
            library::upsert_song(&tx, song, album_rowid)?;
        }
        tx.commit()?;
        search_local(c, &owned, limits)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_building() {
        assert_eq!(
            fts_query("  björk  post "),
            Some("\"björk\"* \"post\"*".into())
        );
        assert_eq!(fts_query("say \"hi\""), Some("\"say\"* \"hi\"*".into()));
        assert_eq!(fts_query("   "), None);
    }
}
