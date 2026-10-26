//! Library types: artists, albums, songs, genres (Subsonic ID3 + OpenSubsonic fields).

use serde::{Deserialize, Deserializer};

use super::time::parse_timestamp;

/// `0` means "not rated" in Subsonic; map it to `None`.
fn rating<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(Option::<i64>::deserialize(d)?.filter(|r| (1..=5).contains(r)))
}

/// Accepts strings; empty strings become `None`.
fn non_empty<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.filter(|s| !s.trim().is_empty()))
}

/// Navidrome sends `0` for unknown numbers (year, track); treat as missing.
fn positive<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(Option::<i64>::deserialize(d)?.filter(|n| *n > 0))
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Contributor {
    pub role: String,
    #[serde(rename = "subRole")]
    pub sub_role: Option<String>,
    pub artist: ArtistRef,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct GenreRef {
    pub name: String,
}

/// OpenSubsonic `replayGain` object. Fields are absent when unknown.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ReplayGain {
    pub track_gain: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_gain: Option<f64>,
    pub album_peak: Option<f64>,
}

/// A song (`Child` in the Subsonic schema).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Song {
    pub id: String,
    pub parent: Option<String>,
    pub title: String,
    pub album: Option<String>,
    #[serde(deserialize_with = "non_empty")]
    pub artist: Option<String>,
    pub album_id: Option<String>,
    pub artist_id: Option<String>,
    #[serde(deserialize_with = "positive")]
    pub track: Option<i64>,
    #[serde(deserialize_with = "positive")]
    pub disc_number: Option<i64>,
    #[serde(deserialize_with = "positive")]
    pub year: Option<i64>,
    pub genre: Option<String>,
    pub cover_art: Option<String>,
    pub size: i64,
    pub content_type: Option<String>,
    pub suffix: Option<String>,
    pub duration: i64,
    pub bit_rate: Option<i64>,
    pub sampling_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub channel_count: Option<i64>,
    pub starred: Option<String>,
    #[serde(deserialize_with = "rating")]
    pub user_rating: Option<i64>,
    pub play_count: Option<i64>,
    pub replay_gain: Option<ReplayGain>,
    pub artists: Vec<ArtistRef>,
    pub album_artists: Vec<ArtistRef>,
    #[serde(deserialize_with = "non_empty")]
    pub display_artist: Option<String>,
    pub contributors: Vec<Contributor>,
    pub sort_name: Option<String>,
}

impl Song {
    pub fn starred_at(&self) -> Option<i64> {
        self.starred.as_deref().and_then(parse_timestamp)
    }

    /// Display string for the artist: OpenSubsonic `displayArtist`, else `artist`.
    pub fn artist_display(&self) -> Option<&str> {
        self.display_artist.as_deref().or(self.artist.as_deref())
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Album {
    pub id: String,
    pub name: String,
    #[serde(deserialize_with = "non_empty")]
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub cover_art: Option<String>,
    pub song_count: i64,
    pub duration: i64,
    pub play_count: Option<i64>,
    pub created: Option<String>,
    pub played: Option<String>,
    #[serde(deserialize_with = "positive")]
    pub year: Option<i64>,
    pub genre: Option<String>,
    pub genres: Vec<GenreRef>,
    pub starred: Option<String>,
    #[serde(deserialize_with = "rating")]
    pub user_rating: Option<i64>,
    pub sort_name: Option<String>,
    pub is_compilation: Option<bool>,
    pub artists: Vec<ArtistRef>,
    #[serde(deserialize_with = "non_empty")]
    pub display_artist: Option<String>,
    /// Present only in `getAlbum`.
    pub song: Vec<Song>,
}

impl Album {
    pub fn created_at(&self) -> Option<i64> {
        self.created.as_deref().and_then(parse_timestamp)
    }

    pub fn played_at(&self) -> Option<i64> {
        self.played.as_deref().and_then(parse_timestamp)
    }

    pub fn starred_at(&self) -> Option<i64> {
        self.starred.as_deref().and_then(parse_timestamp)
    }

    pub fn artist_display(&self) -> Option<&str> {
        self.display_artist.as_deref().or(self.artist.as_deref())
    }

    /// Genre names, from OpenSubsonic `genres` or the legacy `genre` field.
    pub fn genre_names(&self) -> Vec<&str> {
        if self.genres.is_empty() {
            self.genre
                .as_deref()
                .filter(|g| !g.is_empty())
                .into_iter()
                .collect()
        } else {
            self.genres
                .iter()
                .map(|g| g.name.as_str())
                .filter(|g| !g.is_empty())
                .collect()
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub cover_art: Option<String>,
    pub album_count: i64,
    pub starred: Option<String>,
    #[serde(deserialize_with = "rating")]
    pub user_rating: Option<i64>,
    pub sort_name: Option<String>,
    /// Present only in `getArtist`.
    pub album: Vec<Album>,
}

impl Artist {
    pub fn starred_at(&self) -> Option<i64> {
        self.starred.as_deref().and_then(parse_timestamp)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AlbumList2 {
    pub album: Vec<Album>,
}

/// One letter group of `getArtists`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ArtistIndex {
    pub name: String,
    pub artist: Vec<Artist>,
}

/// `getArtists` payload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArtistsIndex {
    pub ignored_articles: String,
    pub last_modified: Option<i64>,
    pub index: Vec<ArtistIndex>,
}

impl ArtistsIndex {
    pub fn artists(&self) -> impl Iterator<Item = &Artist> {
        self.index.iter().flat_map(|i| i.artist.iter())
    }

    /// Articles to ignore when sorting (`ignoredArticles`, space separated).
    pub fn articles(&self) -> Vec<&str> {
        self.ignored_articles.split_whitespace().collect()
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Genre {
    pub value: String,
    pub song_count: i64,
    pub album_count: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Genres {
    pub genre: Vec<Genre>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanStatus {
    pub scanning: bool,
    pub count: i64,
    pub folder_count: i64,
    pub last_scan: Option<String>,
}

impl ScanStatus {
    pub fn last_scan_at(&self) -> Option<i64> {
        self.last_scan.as_deref().and_then(parse_timestamp)
    }
}

/// `getStarred2` and `search3` payloads share this shape.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ItemSet {
    pub artist: Vec<Artist>,
    pub album: Vec<Album>,
    pub song: Vec<Song>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    pub owner: Option<String>,
    pub public: Option<bool>,
    pub song_count: i64,
    pub duration: i64,
    pub created: Option<String>,
    pub changed: Option<String>,
    pub cover_art: Option<String>,
    /// Navidrome extension; when absent, compare `owner` with the username.
    pub readonly: Option<bool>,
    /// Present only in `getPlaylist`.
    pub entry: Vec<Song>,
}

impl Playlist {
    pub fn changed_at(&self) -> Option<i64> {
        self.changed.as_deref().and_then(parse_timestamp)
    }

    pub fn is_readonly_for(&self, username: &str) -> bool {
        self.readonly
            .unwrap_or_else(|| self.owner.as_deref().is_some_and(|o| o != username))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Playlists {
    pub playlist: Vec<Playlist>,
}
