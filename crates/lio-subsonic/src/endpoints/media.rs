use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use url::Url;

use crate::Client;
use crate::client::redact;
use crate::error::{Error, Result};
use crate::types::{ClassicLyrics, LyricsList, StructuredLyrics};

/// Cover sizes Lio requests, so server and disk caches stay small.
pub const COVER_SIZES: [u32; 4] = [64, 192, 512, 1024];

/// An audio response opened by [`Client::open_stream`].
#[derive(Debug)]
pub struct StreamResponse {
    pub response: reqwest::Response,
    /// Byte offset of the first byte in the body (non-zero only for `206`).
    pub start: u64,
    /// Total size of the resource, if the server said (may be an estimate for transcodes).
    pub total: Option<u64>,
    /// Whether the server honoured the `Range` request.
    pub partial: bool,
}

impl Client {
    /// `stream` URL. `format` is `raw` for the original file, else e.g. `opus`/`mp3` with
    /// `max_bit_rate`. Always asks for `estimateContentLength=true`.
    pub fn stream_url(
        &self,
        id: &str,
        format: &str,
        max_bit_rate: Option<u32>,
        time_offset_s: Option<u64>,
    ) -> Url {
        let mut params = vec![
            ("id", id.to_owned()),
            ("format", format.to_owned()),
            ("estimateContentLength", "true".to_owned()),
        ];
        if let Some(rate) = max_bit_rate {
            params.push(("maxBitRate", rate.to_string()));
        }
        if let Some(t) = time_offset_s.filter(|t| *t > 0) {
            params.push(("timeOffset", t.to_string()));
        }
        self.url("stream", &params)
    }

    /// `download` URL: the original file, for pinned downloads at original quality.
    pub fn download_url(&self, id: &str) -> Url {
        self.url("download", &[("id", id.to_owned())])
    }

    /// `getCoverArt` URL at one of [`COVER_SIZES`] (rounded up).
    pub fn cover_art_url(&self, id: &str, size: u32) -> Url {
        let size = COVER_SIZES.into_iter().find(|s| *s >= size).unwrap_or(1024);
        self.url(
            "getCoverArt",
            &[("id", id.to_owned()), ("size", size.to_string())],
        )
    }

    /// `getLyricsBySongId` (OpenSubsonic `songLyrics`): every lyrics set for the song,
    /// synced ones with per-line start times. Empty when the song has none.
    pub async fn get_lyrics_by_song_id(&self, id: &str) -> Result<Vec<StructuredLyrics>> {
        let list: LyricsList = self
            .get("getLyricsBySongId", &[("id", id.to_owned())], "lyricsList")
            .await?;
        Ok(list.structured_lyrics)
    }

    /// Classic `getLyrics`, matched by artist and title. `None` when the server has none.
    pub async fn get_lyrics(&self, artist: &str, title: &str) -> Result<Option<String>> {
        let lyrics: ClassicLyrics = self
            .get(
                "getLyrics",
                &[("artist", artist.to_owned()), ("title", title.to_owned())],
                "lyrics",
            )
            .await?;
        Ok(Some(lyrics.value).filter(|v| !v.trim().is_empty()))
    }

    /// GETs `url`, asking for bytes from `start` onwards when `start > 0`.
    pub async fn open_stream(&self, url: Url, start: u64) -> Result<StreamResponse> {
        tracing::debug!(url = %redact(&url), start, "open stream");
        let mut request = self.http().get(url.clone());
        if start > 0 {
            request = request.header(RANGE, format!("bytes={start}-"));
        }
        let response = request
            .send()
            .await
            .map_err(|e| self.map_transport_error(e))?;
        let status = response.status();
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
        };
        match status {
            StatusCode::PARTIAL_CONTENT => {
                // Content-Range: bytes <start>-<end>/<total|*>
                let (range_start, total) = header(CONTENT_RANGE)
                    .and_then(parse_content_range)
                    .ok_or(Error::Decode("206 without a usable Content-Range".into()))?;
                Ok(StreamResponse {
                    start: range_start,
                    total,
                    partial: true,
                    response,
                })
            }
            s if s.is_success() => {
                if header(reqwest::header::CONTENT_TYPE).is_some_and(|ct| {
                    ct.starts_with("application/json") || ct.starts_with("text/xml")
                }) {
                    // Subsonic errors come back as a 200 JSON envelope.
                    let body = response
                        .bytes()
                        .await
                        .map_err(|e| self.map_transport_error(e))?;
                    let err = serde_json::from_slice::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("subsonic-response")?.get("error").cloned())
                        .map(|e| {
                            Error::from_code(
                                e.get("code").and_then(|c| c.as_i64()).unwrap_or(0) as i32,
                                e.get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("")
                                    .to_owned(),
                            )
                        });
                    return Err(err.unwrap_or(Error::Incompatible));
                }
                let total = header(CONTENT_LENGTH).and_then(|v| v.parse().ok());
                Ok(StreamResponse {
                    start: 0,
                    total,
                    partial: false,
                    response,
                })
            }
            StatusCode::RANGE_NOT_SATISFIABLE => Err(Error::Http(416)),
            StatusCode::NOT_FOUND => Err(Error::NotFound),
            s => Err(Error::Http(s.as_u16())),
        }
    }
}

/// Parses `bytes <start>-<end>/<total>` into `(start, total)`; `*` totals are `None`.
fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    Some((start.parse().ok()?, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range() {
        assert_eq!(
            parse_content_range("bytes 100-199/1000"),
            Some((100, Some(1000)))
        );
        assert_eq!(parse_content_range("bytes 0-0/*"), Some((0, None)));
        assert_eq!(parse_content_range("items 1-2/3"), None);
    }
}
