//! LRCLIB (<https://lrclib.net>) lookup: a free community lyrics database, mostly with
//! timed (LRC) lyrics. Not part of the Subsonic API; Lio asks it when the server has no
//! timed lyrics for a track.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use url::Url;

use crate::client::{TlsConfig, rustls_config};
use crate::error::{Error, Result};

pub const DEFAULT_BASE: &str = "https://lrclib.net";
/// Generous: an exact lookup LRCLIB hasn't seen before can take a while.
const TIMEOUT: Duration = Duration::from_secs(20);
/// How far a search result's duration may be from the track's.
const DURATION_TOLERANCE_S: f64 = 3.0;

/// One LRCLIB record (only the fields Lio uses).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LrclibRecord {
    pub id: i64,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: Option<String>,
    /// Seconds.
    pub duration: f64,
    pub instrumental: bool,
    pub plain_lyrics: Option<String>,
    /// LRC text (`[mm:ss.xx] line`).
    pub synced_lyrics: Option<String>,
}

impl LrclibRecord {
    fn is_synced(&self) -> bool {
        self.synced_lyrics
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    }
}

/// What to look up.
#[derive(Debug, Clone, Default)]
pub struct LyricsQuery<'a> {
    pub artist: &'a str,
    pub title: &'a str,
    pub album: Option<&'a str>,
    pub duration_s: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Lrclib {
    http: reqwest::Client,
    base: Url,
}

impl Lrclib {
    pub fn new() -> Result<Self> {
        Self::with_base(DEFAULT_BASE)
    }

    pub fn with_base(base: &str) -> Result<Self> {
        let base = Url::parse(base).map_err(|e| Error::Decode(format!("bad LRCLIB URL: {e}")))?;
        let tls = rustls_config(&TlsConfig::System, Arc::new(Mutex::new(None)))?;
        let http = reqwest::Client::builder()
            // LRCLIB asks clients to identify themselves.
            .user_agent(concat!(
                "Lio/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/m5ka/lio)"
            ))
            .timeout(TIMEOUT)
            .tls_backend_preconfigured(tls)
            .build()
            .map_err(|e| Error::Decode(format!("HTTP client setup failed: {e}")))?;
        Ok(Self { http, base })
    }

    /// Best record for a track: the exact match (`/api/get`, which needs album and
    /// duration), otherwise a search by artist and title that prefers timed lyrics and
    /// the closest duration. `None` when nothing fits.
    pub async fn find(&self, query: &LyricsQuery<'_>) -> Result<Option<LrclibRecord>> {
        if let (Some(album), Some(duration)) = (query.album, query.duration_s) {
            let exact: Option<LrclibRecord> = self
                .fetch(
                    "api/get",
                    &[
                        ("artist_name", query.artist.to_owned()),
                        ("track_name", query.title.to_owned()),
                        ("album_name", album.to_owned()),
                        ("duration", duration.to_string()),
                    ],
                )
                .await?;
            if let Some(record) = exact.filter(|r| r.is_synced() || r.instrumental) {
                return Ok(Some(record));
            }
        }
        let results: Vec<LrclibRecord> = self
            .fetch(
                "api/search",
                &[
                    ("artist_name", query.artist.to_owned()),
                    ("track_name", query.title.to_owned()),
                ],
            )
            .await?
            .unwrap_or_default();
        Ok(best_match(results, query.duration_s))
    }

    /// GETs `path`; `Ok(None)` for 404.
    async fn fetch<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Option<T>> {
        let mut url = self
            .base
            .join(path)
            .map_err(|e| Error::Decode(e.to_string()))?;
        url.query_pairs_mut().extend_pairs(params);
        tracing::debug!(%path, "LRCLIB lookup");
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|_| Error::Unreachable)?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(None),
            s if s.is_success() => response
                .json()
                .await
                .map(Some)
                .map_err(|e| Error::Decode(format!("LRCLIB: {e}"))),
            s => Err(Error::Http(s.as_u16())),
        }
    }
}

/// Timed lyrics first, then the closest duration; records too far off are dropped
/// (another recording or edit of the song).
fn best_match(results: Vec<LrclibRecord>, duration_s: Option<u64>) -> Option<LrclibRecord> {
    let distance = |r: &LrclibRecord| duration_s.map_or(0.0, |d| (r.duration - d as f64).abs());
    results
        .into_iter()
        .filter(|r| r.is_synced() || r.instrumental || r.plain_lyrics.is_some())
        .filter(|r| distance(r) <= DURATION_TOLERANCE_S)
        .min_by(|a, b| {
            (!a.is_synced())
                .cmp(&!b.is_synced())
                .then(distance(a).total_cmp(&distance(b)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: i64, duration: f64, synced: bool) -> LrclibRecord {
        LrclibRecord {
            id,
            duration,
            plain_lyrics: Some("plain".into()),
            synced_lyrics: synced.then(|| "[00:01.00] a".into()),
            ..Default::default()
        }
    }

    #[test]
    fn prefers_synced_then_closest_duration() {
        let pick = |rs, d| best_match(rs, d).map(|r| r.id);
        assert_eq!(
            pick(
                vec![record(1, 200.0, false), record(2, 202.0, true)],
                Some(200)
            ),
            Some(2)
        );
        assert_eq!(
            pick(
                vec![record(1, 202.5, true), record(2, 199.0, true)],
                Some(200)
            ),
            Some(2)
        );
        assert_eq!(pick(vec![record(1, 260.0, true)], Some(200)), None);
        assert_eq!(pick(vec![record(1, 260.0, true)], None), Some(1));
    }
}
