use serde::Deserialize;

/// Envelope fields of every `subsonic-response`, as returned by `ping`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerInfo {
    pub status: String,
    pub version: String,
    #[serde(rename = "type")]
    pub server_type: String,
    pub server_version: String,
    pub open_subsonic: bool,
}

impl ServerInfo {
    /// Navidrome with OpenSubsonic support: the combination Lio is tested against.
    pub fn is_fully_compatible(&self) -> bool {
        self.open_subsonic && self.server_type.eq_ignore_ascii_case("navidrome")
    }
}

/// One entry of `getOpenSubsonicExtensions`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Extension {
    pub name: String,
    pub versions: Vec<i32>,
}

/// `getUser` payload (only the fields Lio uses).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct User {
    pub username: String,
    pub email: Option<String>,
    pub admin_role: bool,
    pub stream_role: bool,
    pub download_role: bool,
    /// Unreliable on Navidrome (always false); don't gate playlist editing on it.
    pub playlist_role: bool,
    pub cover_art_role: bool,
}

/// OpenSubsonic extension name that enables `apiKey` authentication.
pub const EXT_API_KEY: &str = "apiKeyAuthentication";

/// One set of lyrics from `getLyricsBySongId` (OpenSubsonic `songLyrics`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StructuredLyrics {
    /// ISO 639 code; `xxx` when unknown.
    pub lang: String,
    /// Whether every line has a `start` time.
    pub synced: bool,
    /// Milliseconds to add to every `start`.
    pub offset: i64,
    pub display_artist: Option<String>,
    pub display_title: Option<String>,
    pub line: Vec<LyricLine>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct LyricLine {
    /// Milliseconds from the start of the track (synced lyrics only).
    pub start: Option<i64>,
    pub value: String,
}

/// `lyricsList` payload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LyricsList {
    pub structured_lyrics: Vec<StructuredLyrics>,
}

/// Classic `getLyrics` payload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClassicLyrics {
    pub value: String,
}
