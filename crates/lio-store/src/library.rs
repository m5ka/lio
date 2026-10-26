//! Writes server metadata into the library mirror (artists, albums, tracks, joins, FTS).

use lio_subsonic::types::{Album, Artist, ArtistRef, Song};
use rusqlite::{Connection, OptionalExtension, params};

use crate::db::now;
use crate::error::Result;

/// Articles stripped for sorting when the server doesn't send `sortName`
/// (Navidrome's default `ignoredArticles`).
pub const DEFAULT_IGNORED_ARTICLES: &[&str] = &["The", "El", "La", "Los", "Las", "Le", "Les"];

/// Sort key: lowercase, leading article removed.
pub fn sort_key(name: &str, ignored_articles: &[&str]) -> String {
    let trimmed = name.trim();
    for article in ignored_articles {
        if let Some(rest) = trimmed.strip_prefix(article)
            && let Some(rest) = rest.strip_prefix(' ')
            && !rest.trim().is_empty()
        {
            return rest.trim().to_lowercase();
        }
    }
    trimmed.to_lowercase()
}

fn sort_name(server: Option<&str>, name: &str) -> String {
    match server {
        Some(s) if !s.trim().is_empty() => s.to_lowercase(),
        _ => sort_key(name, DEFAULT_IGNORED_ARTICLES),
    }
}

/// FTS rows are keyed by `ref_rowid * 4 + kind`, so updates are primary-key operations
/// (FTS5 can't index the `kind`/`ref_rowid` columns themselves).
pub fn fts_rowid(kind: &str, rowid: i64) -> i64 {
    let code = match kind {
        "artist" => 1,
        "album" => 2,
        _ => 3,
    };
    rowid * 4 + code
}

fn set_fts(conn: &Connection, kind: &str, rowid: i64, text: &str) -> Result<()> {
    let key = fts_rowid(kind, rowid);
    conn.prepare_cached("DELETE FROM search_fts WHERE rowid = ?1")?
        .execute([key])?;
    conn.prepare_cached(
        "INSERT INTO search_fts (rowid, kind, ref_rowid, text) VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute(params![key, kind, rowid, text])?;
    Ok(())
}

/// Removes FTS rows whose referenced row no longer exists.
pub fn prune_fts(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM search_fts WHERE rowid IN (
            SELECT f.rowid FROM search_fts f WHERE
               (f.rowid % 4 = 1 AND f.rowid / 4 NOT IN (SELECT rowid FROM artist))
            OR (f.rowid % 4 = 2 AND f.rowid / 4 NOT IN (SELECT rowid FROM album))
            OR (f.rowid % 4 = 3 AND f.rowid / 4 NOT IN (SELECT rowid FROM track)));",
    )?;
    Ok(())
}

/// Ensures an artist row exists for `artist` (name-only if new); returns its rowid.
fn ensure_artist_ref(conn: &Connection, artist: &ArtistRef) -> Result<Option<i64>> {
    if artist.id.is_empty() {
        return Ok(None);
    }
    let rowid = conn
        .query_row(
            "INSERT INTO artist (id, name, sort_name) VALUES (?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name
             RETURNING rowid",
            params![
                artist.id,
                artist.name,
                sort_key(&artist.name, DEFAULT_IGNORED_ARTICLES)
            ],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(rowid) = rowid {
        set_fts(conn, "artist", rowid, &artist.name)?;
    }
    Ok(rowid)
}

/// Upserts a full artist (from `getArtists` / `getArtist`); returns its rowid.
pub fn upsert_artist(conn: &Connection, artist: &Artist) -> Result<i64> {
    let rowid: i64 = conn.query_row(
        "INSERT INTO artist (id, name, sort_name, album_count, cover_art_id, starred_at, rating, synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (id) DO UPDATE SET
            name = excluded.name, sort_name = excluded.sort_name,
            album_count = excluded.album_count, cover_art_id = excluded.cover_art_id,
            starred_at = excluded.starred_at, rating = excluded.rating,
            synced_at = excluded.synced_at
         RETURNING rowid",
        params![
            artist.id,
            artist.name,
            sort_name(artist.sort_name.as_deref(), &artist.name),
            artist.album_count,
            artist.cover_art,
            artist.starred_at(),
            artist.user_rating,
            now(),
        ],
        |r| r.get(0),
    )?;
    set_fts(conn, "artist", rowid, &artist.name)?;
    Ok(rowid)
}

/// Upserts album metadata (no tracks); returns its rowid. Keeps `tracks_synced_at`.
pub fn upsert_album(conn: &Connection, album: &Album) -> Result<i64> {
    let rowid: i64 = conn.query_row(
        "INSERT INTO album (id, name, sort_name, artist_display, year, genre, cover_art_id,
                            song_count, duration_s, play_count, created_at, played_at,
                            starred_at, rating, is_compilation, synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT (id) DO UPDATE SET
            name = excluded.name, sort_name = excluded.sort_name,
            artist_display = excluded.artist_display, year = excluded.year,
            genre = excluded.genre, cover_art_id = excluded.cover_art_id,
            song_count = excluded.song_count, duration_s = excluded.duration_s,
            play_count = excluded.play_count, created_at = excluded.created_at,
            played_at = excluded.played_at, starred_at = excluded.starred_at,
            rating = excluded.rating, is_compilation = excluded.is_compilation,
            synced_at = excluded.synced_at
         RETURNING rowid",
        params![
            album.id,
            album.name,
            sort_name(album.sort_name.as_deref(), &album.name),
            album.artist_display(),
            album.year,
            album.genre_names().first().copied(),
            album.cover_art,
            album.song_count,
            album.duration,
            album.play_count.unwrap_or(0),
            album.created_at().unwrap_or(0),
            album.played_at(),
            album.starred_at(),
            album.user_rating,
            album.is_compilation.unwrap_or(false),
            now(),
        ],
        |r| r.get(0),
    )?;

    conn.execute("DELETE FROM album_genre WHERE album_rowid = ?1", [rowid])?;
    for genre in album.genre_names() {
        conn.execute(
            "INSERT OR IGNORE INTO album_genre (album_rowid, genre) VALUES (?1, ?2)",
            params![rowid, genre],
        )?;
    }

    conn.execute("DELETE FROM album_artist WHERE album_rowid = ?1", [rowid])?;
    let mut artists: Vec<ArtistRef> = album.artists.clone();
    if artists.is_empty()
        && let (Some(id), Some(name)) = (&album.artist_id, &album.artist)
    {
        artists.push(ArtistRef {
            id: id.clone(),
            name: name.clone(),
        });
    }
    for artist in &artists {
        if let Some(artist_rowid) = ensure_artist_ref(conn, artist)? {
            conn.execute(
                "INSERT OR IGNORE INTO album_artist (album_rowid, artist_rowid, role)
                 VALUES (?1, ?2, 'albumartist')",
                params![rowid, artist_rowid],
            )?;
        }
    }

    let fts_text = match album.artist_display() {
        Some(artist) => format!("{} {artist}", album.name),
        None => album.name.clone(),
    };
    set_fts(conn, "album", rowid, &fts_text)?;
    Ok(rowid)
}

/// Upserts one song; returns its rowid.
pub fn upsert_song(conn: &Connection, song: &Song, album_rowid: Option<i64>) -> Result<i64> {
    let rg = song.replay_gain.unwrap_or_default();
    let rowid: i64 = conn.query_row(
        "INSERT INTO track (id, album_rowid, title, artist_display, track_no, disc_no, duration_s,
                            suffix, content_type, bitrate_kbps, sample_rate, bit_depth, channels,
                            size_bytes, rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak,
                            play_count, starred_at, rating, cover_art_id, synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 ?19, ?20, ?21, ?22, ?23)
         ON CONFLICT (id) DO UPDATE SET
            album_rowid = COALESCE(excluded.album_rowid, track.album_rowid),
            title = excluded.title, artist_display = excluded.artist_display,
            track_no = excluded.track_no, disc_no = excluded.disc_no,
            duration_s = excluded.duration_s, suffix = excluded.suffix,
            content_type = excluded.content_type, bitrate_kbps = excluded.bitrate_kbps,
            sample_rate = excluded.sample_rate, bit_depth = excluded.bit_depth,
            channels = excluded.channels, size_bytes = excluded.size_bytes,
            rg_track_gain = excluded.rg_track_gain, rg_track_peak = excluded.rg_track_peak,
            rg_album_gain = excluded.rg_album_gain, rg_album_peak = excluded.rg_album_peak,
            play_count = excluded.play_count, starred_at = excluded.starred_at,
            rating = excluded.rating, cover_art_id = excluded.cover_art_id,
            synced_at = excluded.synced_at
         RETURNING rowid",
        params![
            song.id,
            album_rowid,
            song.title,
            song.artist_display(),
            song.track,
            song.disc_number,
            song.duration,
            song.suffix,
            song.content_type,
            song.bit_rate,
            song.sampling_rate,
            song.bit_depth,
            song.channel_count,
            song.size,
            rg.track_gain,
            rg.track_peak,
            rg.album_gain,
            rg.album_peak,
            song.play_count.unwrap_or(0),
            song.starred_at(),
            song.user_rating,
            song.cover_art,
            now(),
        ],
        |r| r.get(0),
    )?;

    conn.execute("DELETE FROM track_artist WHERE track_rowid = ?1", [rowid])?;
    let mut refs: Vec<(&ArtistRef, &str)> = song.artists.iter().map(|a| (a, "artist")).collect();
    refs.extend(
        song.contributors
            .iter()
            .map(|c| (&c.artist, c.role.as_str()))
            .filter(|(_, r)| !r.is_empty()),
    );
    for (artist, role) in refs {
        if let Some(artist_rowid) = ensure_artist_ref(conn, artist)? {
            conn.execute(
                "INSERT OR IGNORE INTO track_artist (track_rowid, artist_rowid, role)
                 VALUES (?1, ?2, ?3)",
                params![rowid, artist_rowid, role],
            )?;
        }
    }

    let fts_text = match (song.artist_display(), song.album.as_deref()) {
        (Some(artist), Some(album)) => format!("{} {artist} {album}", song.title),
        (Some(artist), None) => format!("{} {artist}", song.title),
        _ => song.title.clone(),
    };
    set_fts(conn, "track", rowid, &fts_text)?;
    Ok(rowid)
}

/// Upserts an album from `getAlbum` together with all its songs, removing songs that
/// are no longer on the album. Returns the album rowid.
pub fn upsert_album_with_tracks(conn: &mut Connection, album: &Album) -> Result<i64> {
    let tx = conn.transaction()?;
    let album_rowid = upsert_album(&tx, album)?;
    let mut keep = Vec::with_capacity(album.song.len());
    for song in &album.song {
        keep.push(upsert_song(&tx, song, Some(album_rowid))?);
    }
    // Detach (rather than delete) songs that left this album: they may live in playlists.
    let placeholders = vec!["?"; keep.len()].join(",");
    let sql = if keep.is_empty() {
        "UPDATE track SET album_rowid = NULL WHERE album_rowid = ?1".to_owned()
    } else {
        format!(
            "UPDATE track SET album_rowid = NULL WHERE album_rowid = ?1 AND rowid NOT IN ({placeholders})"
        )
    };
    let mut values: Vec<i64> = vec![album_rowid];
    values.extend(&keep);
    tx.execute(&sql, rusqlite::params_from_iter(values))?;
    tx.execute(
        "UPDATE album SET tracks_synced_at = ?1 WHERE rowid = ?2",
        params![now(), album_rowid],
    )?;
    tx.commit()?;
    Ok(album_rowid)
}

/// Upserts a batch of albums (one `getAlbumList2` page) in one transaction.
pub fn upsert_albums(conn: &mut Connection, albums: &[Album]) -> Result<Vec<i64>> {
    let tx = conn.transaction()?;
    let mut rowids = Vec::with_capacity(albums.len());
    for album in albums {
        rowids.push(upsert_album(&tx, album)?);
    }
    tx.commit()?;
    Ok(rowids)
}

/// Upserts playlist metadata; returns its rowid.
pub fn upsert_playlist(
    conn: &Connection,
    playlist: &lio_subsonic::types::Playlist,
    username: &str,
) -> Result<i64> {
    let rowid = conn.query_row(
        "INSERT INTO playlist (id, name, comment, owner, public, song_count, duration_s, changed_at,
                               cover_art_id, readonly)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (id) DO UPDATE SET
            name = excluded.name, comment = excluded.comment, owner = excluded.owner,
            public = excluded.public, song_count = excluded.song_count,
            duration_s = excluded.duration_s, changed_at = excluded.changed_at,
            cover_art_id = excluded.cover_art_id, readonly = excluded.readonly
         RETURNING rowid",
        params![
            playlist.id,
            playlist.name,
            playlist.comment,
            playlist.owner,
            playlist.public.unwrap_or(false),
            playlist.song_count,
            playlist.duration,
            playlist.changed_at().unwrap_or(0),
            playlist.cover_art,
            playlist.is_readonly_for(username),
        ],
        |r| r.get(0),
    )?;
    Ok(rowid)
}

/// Replaces a playlist's entries (from `getPlaylist`), upserting the songs.
pub fn replace_playlist_tracks(
    conn: &mut Connection,
    playlist: &lio_subsonic::types::Playlist,
    username: &str,
) -> Result<i64> {
    let tx = conn.transaction()?;
    let rowid = upsert_playlist(&tx, playlist, username)?;
    tx.execute(
        "DELETE FROM playlist_track WHERE playlist_rowid = ?1",
        [rowid],
    )?;
    for (position, song) in playlist.entry.iter().enumerate() {
        let album_rowid: Option<i64> = match &song.album_id {
            Some(id) => tx
                .query_row("SELECT rowid FROM album WHERE id = ?1", [id], |r| r.get(0))
                .optional()?,
            None => None,
        };
        let track_rowid = upsert_song(&tx, song, album_rowid)?;
        tx.execute(
            "INSERT INTO playlist_track (playlist_rowid, position, track_rowid) VALUES (?1, ?2, ?3)",
            params![rowid, position as i64, track_rowid],
        )?;
    }
    tx.execute(
        "UPDATE playlist SET tracks_synced_at = ?1 WHERE rowid = ?2",
        params![now(), rowid],
    )?;
    tx.commit()?;
    Ok(rowid)
}

/// Applies `getStarred2`: everything starred on the server is starred locally, nothing else.
pub fn apply_starred(conn: &mut Connection, starred: &lio_subsonic::types::ItemSet) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        "UPDATE artist SET starred_at = NULL WHERE starred_at IS NOT NULL;
         UPDATE album SET starred_at = NULL WHERE starred_at IS NOT NULL;
         UPDATE track SET starred_at = NULL WHERE starred_at IS NOT NULL;",
    )?;
    // Only touch `starred_at` on rows we already have, so a sparse payload can't
    // overwrite richer metadata; insert rows we don't know yet.
    for artist in &starred.artist {
        let updated = tx.execute(
            "UPDATE artist SET starred_at = ?1 WHERE id = ?2",
            params![artist.starred_at(), artist.id],
        )?;
        if updated == 0 {
            upsert_artist(&tx, artist)?;
        }
    }
    for album in &starred.album {
        let updated = tx.execute(
            "UPDATE album SET starred_at = ?1 WHERE id = ?2",
            params![album.starred_at(), album.id],
        )?;
        if updated == 0 {
            upsert_album(&tx, album)?;
        }
    }
    for song in &starred.song {
        let updated = tx.execute(
            "UPDATE track SET starred_at = ?1 WHERE id = ?2",
            params![song.starred_at(), song.id],
        )?;
        if updated == 0 {
            let album_rowid: Option<i64> = match &song.album_id {
                Some(id) => tx
                    .query_row("SELECT rowid FROM album WHERE id = ?1", [id], |r| r.get(0))
                    .optional()?,
                None => None,
            };
            upsert_song(&tx, song, album_rowid)?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Reads a `sync_state` value.
pub fn sync_value(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM sync_state WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn set_sync_value(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_state (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_keys() {
        assert_eq!(sort_key("The Beatles", DEFAULT_IGNORED_ARTICLES), "beatles");
        assert_eq!(sort_key("Theatre", DEFAULT_IGNORED_ARTICLES), "theatre");
        assert_eq!(sort_key("The", DEFAULT_IGNORED_ARTICLES), "the");
        assert_eq!(sort_key("Los Lobos", DEFAULT_IGNORED_ARTICLES), "lobos");
        assert_eq!(sort_name(Some("Abba"), "ABBA"), "abba");
    }
}
