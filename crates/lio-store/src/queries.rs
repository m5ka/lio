//! Read queries for the UI and playback.

use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;
use crate::model::{AlbumRow, TrackRow};

/// Tracks of an album in disc/track order.
pub fn album_track_rows(conn: &Connection, album_rowid: i64) -> Result<Vec<TrackRow>> {
    let sql = format!(
        "SELECT {} FROM {} WHERE track.album_rowid = ?1
         ORDER BY COALESCE(track.disc_no, 1), COALESCE(track.track_no, 0), track.title",
        TrackRow::COLUMNS,
        TrackRow::FROM
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt
        .query_map([album_rowid], TrackRow::from_row)?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

pub fn track_row(conn: &Connection, rowid: i64) -> Result<Option<TrackRow>> {
    let sql = format!(
        "SELECT {} FROM {} WHERE track.rowid = ?1",
        TrackRow::COLUMNS,
        TrackRow::FROM
    );
    Ok(conn
        .query_row(&sql, [rowid], TrackRow::from_row)
        .optional()?)
}

pub fn track_row_by_id(conn: &Connection, id: &str) -> Result<Option<TrackRow>> {
    let sql = format!(
        "SELECT {} FROM {} WHERE track.id = ?1",
        TrackRow::COLUMNS,
        TrackRow::FROM
    );
    Ok(conn.query_row(&sql, [id], TrackRow::from_row).optional()?)
}

pub fn album_row(conn: &Connection, rowid: i64) -> Result<Option<AlbumRow>> {
    let sql = format!(
        "SELECT {} FROM album WHERE album.rowid = ?1",
        AlbumRow::COLUMNS
    );
    Ok(conn
        .query_row(&sql, [rowid], AlbumRow::from_row)
        .optional()?)
}

pub fn album_row_by_id(conn: &Connection, id: &str) -> Result<Option<AlbumRow>> {
    let sql = format!(
        "SELECT {} FROM album WHERE album.id = ?1",
        AlbumRow::COLUMNS
    );
    Ok(conn.query_row(&sql, [id], AlbumRow::from_row).optional()?)
}

/// Album rows for `rowids`, in the given order.
pub fn album_rows(conn: &Connection, rowids: &[i64]) -> Result<Vec<AlbumRow>> {
    rows_in_order(
        conn,
        rowids,
        &format!(
            "SELECT {} FROM album WHERE album.rowid IN",
            AlbumRow::COLUMNS
        ),
        AlbumRow::from_row,
        |r| r.rowid,
    )
}

/// Track rows for `rowids`, in the given order.
pub fn track_rows(conn: &Connection, rowids: &[i64]) -> Result<Vec<TrackRow>> {
    rows_in_order(
        conn,
        rowids,
        &format!(
            "SELECT {} FROM {} WHERE track.rowid IN",
            TrackRow::COLUMNS,
            TrackRow::FROM
        ),
        TrackRow::from_row,
        |r| r.rowid,
    )
}

pub(crate) fn rows_in_order<T>(
    conn: &Connection,
    rowids: &[i64],
    select_in: &str,
    map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    key: fn(&T) -> i64,
) -> Result<Vec<T>> {
    if rowids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; rowids.len()].join(",");
    let mut stmt = conn.prepare(&format!("{select_in} ({placeholders})"))?;
    let mut by_id: std::collections::HashMap<i64, T> = stmt
        .query_map(rusqlite::params_from_iter(rowids), map)?
        .filter_map(|r| r.ok())
        .map(|row| (key(&row), row))
        .collect();
    Ok(rowids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// Album orderings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlbumSort {
    #[default]
    RecentlyAdded,
    Name,
    Artist,
    Year,
    MostPlayed,
    Random,
    RecentlyPlayed,
}

/// Album subsets.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AlbumFilter {
    #[default]
    All,
    Genre(String),
    Starred,
    RecentlyPlayed,
    Artist(i64),
}

/// Rowids of albums matching `filter` in `sort` order (cheap: ~5 ms for 10k albums).
pub fn album_ids(
    conn: &Connection,
    sort: AlbumSort,
    filter: &AlbumFilter,
    limit: Option<usize>,
) -> Result<Vec<i64>> {
    let (filter_sql, param): (&str, Option<rusqlite::types::Value>) = match filter {
        AlbumFilter::All => ("1", None),
        AlbumFilter::Genre(g) => (
            "album.rowid IN (SELECT album_rowid FROM album_genre WHERE genre = ?1)",
            Some(g.clone().into()),
        ),
        AlbumFilter::Starred => ("album.starred_at IS NOT NULL", None),
        AlbumFilter::RecentlyPlayed => ("album.played_at IS NOT NULL", None),
        AlbumFilter::Artist(rowid) => (
            "album.rowid IN (SELECT album_rowid FROM album_artist WHERE artist_rowid = ?1)",
            Some((*rowid).into()),
        ),
    };
    let order = match sort {
        AlbumSort::RecentlyAdded => "album.created_at DESC, album.rowid DESC",
        AlbumSort::Name => "COALESCE(album.sort_name, album.name) COLLATE NOCASE, album.rowid",
        AlbumSort::Artist => {
            "LOWER(COALESCE(album.artist_display, '')), album.year, COALESCE(album.sort_name, album.name) COLLATE NOCASE"
        }
        AlbumSort::Year => {
            "album.year IS NULL, album.year DESC, COALESCE(album.sort_name, album.name) COLLATE NOCASE"
        }
        AlbumSort::MostPlayed => "album.play_count DESC, album.played_at DESC",
        AlbumSort::Random => "random()",
        AlbumSort::RecentlyPlayed => "album.played_at IS NULL, album.played_at DESC",
    };
    let limit_sql = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
    let sql =
        format!("SELECT album.rowid FROM album WHERE {filter_sql} ORDER BY {order}{limit_sql}");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = match param {
        Some(p) => stmt
            .query_map([p], |r| r.get(0))?
            .collect::<Result<_, _>>()?,
        None => stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?,
    };
    Ok(rows)
}

/// Artists that have albums, by sort name; `starred_only` for Favourites.
pub fn artist_ids(conn: &Connection, starred_only: bool) -> Result<Vec<i64>> {
    let sql = format!(
        "SELECT artist.rowid FROM artist WHERE {} ORDER BY COALESCE(artist.sort_name, artist.name) COLLATE NOCASE",
        if starred_only {
            "artist.starred_at IS NOT NULL"
        } else {
            "artist.album_count > 0 OR artist.rowid IN (SELECT artist_rowid FROM album_artist)"
        }
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

pub fn artist_rows(conn: &Connection, rowids: &[i64]) -> Result<Vec<crate::model::ArtistRow>> {
    use crate::model::ArtistRow;
    rows_in_order(
        conn,
        rowids,
        &format!(
            "SELECT {} FROM artist WHERE artist.rowid IN",
            ArtistRow::COLUMNS
        ),
        ArtistRow::from_row,
        |r| r.rowid,
    )
}

pub fn artist_row_by_id(conn: &Connection, id: &str) -> Result<Option<crate::model::ArtistRow>> {
    use crate::model::ArtistRow;
    let sql = format!(
        "SELECT {} FROM artist WHERE artist.id = ?1",
        ArtistRow::COLUMNS
    );
    Ok(conn.query_row(&sql, [id], ArtistRow::from_row).optional()?)
}

/// `(genre, album count)` by name.
pub fn genres(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT genre, COUNT(*) FROM album_genre GROUP BY genre ORDER BY genre COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

pub fn playlists(conn: &Connection) -> Result<Vec<crate::model::PlaylistRow>> {
    use crate::model::PlaylistRow;
    let sql = format!(
        "SELECT {} FROM playlist ORDER BY playlist.name COLLATE NOCASE",
        PlaylistRow::COLUMNS
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt
        .query_map([], PlaylistRow::from_row)?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

pub fn playlist_row(conn: &Connection, rowid: i64) -> Result<Option<crate::model::PlaylistRow>> {
    use crate::model::PlaylistRow;
    let sql = format!(
        "SELECT {} FROM playlist WHERE playlist.rowid = ?1",
        PlaylistRow::COLUMNS
    );
    Ok(conn
        .query_row(&sql, [rowid], PlaylistRow::from_row)
        .optional()?)
}

/// Track rowids of a playlist in order (duplicates allowed).
pub fn playlist_track_ids(conn: &Connection, playlist_rowid: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT track_rowid FROM playlist_track WHERE playlist_rowid = ?1 ORDER BY position",
    )?;
    let rows = stmt
        .query_map([playlist_rowid], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Playlist tracks in order, duplicates preserved.
pub fn playlist_track_rows(conn: &Connection, playlist_rowid: i64) -> Result<Vec<TrackRow>> {
    let ids = playlist_track_ids(conn, playlist_rowid)?;
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    let rows = track_rows(conn, &unique)?;
    let by_id: std::collections::HashMap<i64, TrackRow> =
        rows.into_iter().map(|r| (r.rowid, r)).collect();
    Ok(ids.iter().filter_map(|id| by_id.get(id).cloned()).collect())
}

/// Starred tracks, most recently starred first.
pub fn starred_track_ids(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT rowid FROM track WHERE starred_at IS NOT NULL ORDER BY starred_at DESC",
    )?;
    let rows = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// All tracks of an artist's albums, album by album.
pub fn artist_track_rows(conn: &Connection, artist_rowid: i64) -> Result<Vec<TrackRow>> {
    let sql = format!(
        "SELECT {} FROM {} WHERE track.album_rowid IN
            (SELECT album_rowid FROM album_artist WHERE artist_rowid = ?1)
         ORDER BY album.year, album.sort_name, COALESCE(track.disc_no, 1), track.track_no",
        TrackRow::COLUMNS,
        TrackRow::FROM
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt
        .query_map([artist_rowid], TrackRow::from_row)?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Whether some albums still lack their track list (background sync incomplete).
pub fn tracks_sync_incomplete(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM album WHERE tracks_synced_at IS NULL)",
        [],
        |r| r.get(0),
    )?)
}

pub fn album_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM album", [], |r| r.get(0))?)
}

pub fn track_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM track", [], |r| r.get(0))?)
}
