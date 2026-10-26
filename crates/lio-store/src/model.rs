//! Domain rows shared by the store, playback and UI layers.

use rusqlite::Row;

/// Audio quality of a stream, cache entry or download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Quality {
    #[default]
    Original,
    Opus192,
    Opus128,
    Mp3_320,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Self::Original, Self::Opus192, Self::Opus128, Self::Mp3_320];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Opus192 => "opus-192",
            Self::Opus128 => "opus-128",
            Self::Mp3_320 => "mp3-320",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|q| q.as_str() == s)
    }

    /// `(format, maxBitRate)` for the `stream` endpoint. `original` → `format=raw`.
    pub fn stream_params(self) -> (&'static str, Option<u32>) {
        match self {
            Self::Original => ("raw", None),
            Self::Opus192 => ("opus", Some(192)),
            Self::Opus128 => ("opus", Some(128)),
            Self::Mp3_320 => ("mp3", Some(320)),
        }
    }

    pub fn is_transcoded(self) -> bool {
        self != Self::Original
    }

    /// Preference when falling back to a cached copy at another quality:
    /// `original` first, then higher bitrate.
    pub fn preference(self) -> u8 {
        match self {
            Self::Original => 0,
            Self::Mp3_320 => 1,
            Self::Opus192 => 2,
            Self::Opus128 => 3,
        }
    }
}

impl std::fmt::Display for Quality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtistRow {
    pub rowid: i64,
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub album_count: i64,
    pub cover_art_id: Option<String>,
    pub starred_at: Option<i64>,
    pub rating: Option<i64>,
}

impl ArtistRow {
    pub const COLUMNS: &'static str = "artist.rowid, artist.id, artist.name, artist.sort_name, artist.album_count, \
         artist.cover_art_id, artist.starred_at, artist.rating";

    pub fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            rowid: r.get(0)?,
            id: r.get(1)?,
            name: r.get(2)?,
            sort_name: r.get(3)?,
            album_count: r.get(4)?,
            cover_art_id: r.get(5)?,
            starred_at: r.get(6)?,
            rating: r.get(7)?,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlbumRow {
    pub rowid: i64,
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub artist_display: Option<String>,
    pub year: Option<i64>,
    pub genre: Option<String>,
    pub cover_art_id: Option<String>,
    pub song_count: i64,
    pub duration_s: i64,
    pub play_count: i64,
    pub created_at: i64,
    pub played_at: Option<i64>,
    pub starred_at: Option<i64>,
    pub rating: Option<i64>,
    pub is_compilation: bool,
    pub tracks_synced_at: Option<i64>,
    /// Server ID of the first album artist, for "go to artist".
    pub artist_id: Option<String>,
}

impl AlbumRow {
    pub const COLUMNS: &'static str = "album.rowid, album.id, album.name, album.sort_name, album.artist_display, album.year, \
         album.genre, album.cover_art_id, album.song_count, album.duration_s, album.play_count, \
         album.created_at, album.played_at, album.starred_at, album.rating, album.is_compilation, \
         album.tracks_synced_at, \
         (SELECT artist.id FROM album_artist JOIN artist ON artist.rowid = album_artist.artist_rowid \
          WHERE album_artist.album_rowid = album.rowid ORDER BY album_artist.role LIMIT 1)";

    pub fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            rowid: r.get(0)?,
            id: r.get(1)?,
            name: r.get(2)?,
            sort_name: r.get(3)?,
            artist_display: r.get(4)?,
            year: r.get(5)?,
            genre: r.get(6)?,
            cover_art_id: r.get(7)?,
            song_count: r.get(8)?,
            duration_s: r.get(9)?,
            play_count: r.get(10)?,
            created_at: r.get(11)?,
            played_at: r.get(12)?,
            starred_at: r.get(13)?,
            rating: r.get(14)?,
            is_compilation: r.get(15)?,
            tracks_synced_at: r.get(16)?,
            artist_id: r.get(17)?,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackRow {
    pub rowid: i64,
    pub id: String,
    pub album_rowid: Option<i64>,
    pub album_id: Option<String>,
    pub album_name: Option<String>,
    pub album_artist: Option<String>,
    pub title: String,
    pub artist_display: Option<String>,
    pub track_no: Option<i64>,
    pub disc_no: Option<i64>,
    pub duration_s: i64,
    pub suffix: Option<String>,
    pub content_type: Option<String>,
    pub bitrate_kbps: Option<i64>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub channels: Option<i64>,
    pub size_bytes: i64,
    pub rg_track_gain: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_gain: Option<f64>,
    pub rg_album_peak: Option<f64>,
    pub play_count: i64,
    pub starred_at: Option<i64>,
    pub rating: Option<i64>,
    pub cover_art_id: Option<String>,
    /// Has a complete download or a complete cache entry.
    pub offline_available: bool,
}

impl TrackRow {
    pub const COLUMNS: &'static str = "track.rowid, track.id, track.album_rowid, album.id, album.name, album.artist_display, \
         track.title, track.artist_display, track.track_no, track.disc_no, track.duration_s, \
         track.suffix, track.content_type, track.bitrate_kbps, track.sample_rate, track.bit_depth, \
         track.channels, track.size_bytes, track.rg_track_gain, track.rg_track_peak, \
         track.rg_album_gain, track.rg_album_peak, track.play_count, track.starred_at, \
         track.rating, COALESCE(track.cover_art_id, album.cover_art_id), \
         (EXISTS (SELECT 1 FROM download_file f WHERE f.track_rowid = track.rowid AND f.complete) \
          OR EXISTS (SELECT 1 FROM cache_entry c WHERE c.track_rowid = track.rowid AND c.complete))";

    /// `FROM` clause matching [`Self::COLUMNS`].
    pub const FROM: &'static str = "track LEFT JOIN album ON album.rowid = track.album_rowid";

    pub fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            rowid: r.get(0)?,
            id: r.get(1)?,
            album_rowid: r.get(2)?,
            album_id: r.get(3)?,
            album_name: r.get(4)?,
            album_artist: r.get(5)?,
            title: r.get(6)?,
            artist_display: r.get(7)?,
            track_no: r.get(8)?,
            disc_no: r.get(9)?,
            duration_s: r.get(10)?,
            suffix: r.get(11)?,
            content_type: r.get(12)?,
            bitrate_kbps: r.get(13)?,
            sample_rate: r.get(14)?,
            bit_depth: r.get(15)?,
            channels: r.get(16)?,
            size_bytes: r.get(17)?,
            rg_track_gain: r.get(18)?,
            rg_track_peak: r.get(19)?,
            rg_album_gain: r.get(20)?,
            rg_album_peak: r.get(21)?,
            play_count: r.get(22)?,
            starred_at: r.get(23)?,
            rating: r.get(24)?,
            cover_art_id: r.get(25)?,
            offline_available: r.get(26)?,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlaylistRow {
    pub rowid: i64,
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    pub owner: Option<String>,
    pub public: bool,
    pub song_count: i64,
    pub duration_s: i64,
    pub changed_at: i64,
    pub cover_art_id: Option<String>,
    /// Owned by someone else: hide edit controls.
    pub readonly: bool,
}

impl PlaylistRow {
    pub const COLUMNS: &'static str = "playlist.rowid, playlist.id, playlist.name, playlist.comment, playlist.owner, \
         playlist.public, playlist.song_count, playlist.duration_s, playlist.changed_at, \
         playlist.cover_art_id, playlist.readonly";

    pub fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            rowid: r.get(0)?,
            id: r.get(1)?,
            name: r.get(2)?,
            comment: r.get(3)?,
            owner: r.get(4)?,
            public: r.get(5)?,
            song_count: r.get(6)?,
            duration_s: r.get(7)?,
            changed_at: r.get(8)?,
            cover_art_id: r.get(9)?,
            readonly: r.get(10)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_round_trip_and_params() {
        for q in Quality::ALL {
            assert_eq!(Quality::parse(q.as_str()), Some(q));
        }
        assert_eq!(Quality::Original.stream_params(), ("raw", None));
        assert_eq!(Quality::Opus128.stream_params(), ("opus", Some(128)));
        assert!(Quality::Original.preference() < Quality::Mp3_320.preference());
        assert!(Quality::Opus192.preference() < Quality::Opus128.preference());
    }
}
