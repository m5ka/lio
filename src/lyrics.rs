//! Lyrics for the Now Playing screen.
//!
//! Lookup order: the server's timed lyrics (OpenSubsonic `getLyricsBySongId`), then
//! timed lyrics from LRCLIB (when "Find Lyrics Online" is on), then the server's plain
//! lyrics (`getLyricsBySongId` or classic `getLyrics`), then LRCLIB's plain lyrics.
//! Results are kept in memory for the session (not stored offline).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use gtk::prelude::*;
use lio_subsonic::lrclib::{Lrclib, LyricsQuery};
use lio_subsonic::types::StructuredLyrics;

use crate::application::LioApplication;
use crate::models::Track;
use crate::runtime;

/// Tracks whose lookup result is remembered (lyrics or "none").
const CACHE_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Server,
    Lrclib,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Start in milliseconds (synced lyrics only).
    pub start_ms: Option<u64>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lyrics {
    pub synced: bool,
    pub lines: Vec<Line>,
    pub source: Source,
}

impl Lyrics {
    /// Picks the set to show: synced before unsynced, otherwise the server's order.
    pub fn from_structured(sets: Vec<StructuredLyrics>) -> Option<Self> {
        let set = sets
            .iter()
            .find(|s| s.synced && !s.line.is_empty())
            .or_else(|| sets.iter().find(|s| !s.line.is_empty()))?;
        let lines = set
            .line
            .iter()
            .map(|l| Line {
                start_ms: set
                    .synced
                    .then(|| l.start.map(|s| (s + set.offset).max(0) as u64))
                    .flatten(),
                text: l.value.trim_end().to_owned(),
            })
            .collect();
        Some(Self {
            synced: set.synced,
            lines,
            source: Source::Server,
        })
    }

    /// Plain text, one line per line (blank lines separate stanzas).
    pub fn from_plain(text: &str, source: Source) -> Option<Self> {
        let lines: Vec<Line> = text
            .trim()
            .lines()
            .map(|l| Line {
                start_ms: None,
                text: l.trim_end().to_owned(),
            })
            .collect();
        (!lines.is_empty()).then_some(Self {
            synced: false,
            lines,
            source,
        })
    }

    /// Parses LRC: `[mm:ss.xx]` stamps (several per line allowed), an optional
    /// `[offset:±ms]` tag (positive shows lyrics earlier), other tags ignored and
    /// enhanced-LRC word stamps (`<mm:ss.xx>`) stripped. `None` without timed lines.
    pub fn from_lrc(text: &str, source: Source) -> Option<Self> {
        let mut offset_ms = 0i64;
        let mut timed: Vec<(i64, String)> = Vec::new();
        for raw in text.lines() {
            let mut rest = raw.trim();
            let mut stamps = Vec::new();
            while rest.starts_with('[') {
                let Some(end) = rest.find(']') else { break };
                let tag = &rest[1..end];
                rest = &rest[end + 1..];
                if let Some(ms) = parse_stamp(tag) {
                    stamps.push(ms);
                } else if let Some(value) = tag.strip_prefix("offset:") {
                    offset_ms = value.trim().parse().unwrap_or(0);
                }
            }
            let line = strip_word_stamps(rest.trim());
            timed.extend(stamps.into_iter().map(|s| (s, line.clone())));
        }
        if timed.is_empty() {
            return None;
        }
        timed.sort_by_key(|(start, _)| *start);
        Some(Self {
            synced: true,
            lines: timed
                .into_iter()
                .map(|(start, text)| Line {
                    start_ms: Some((start - offset_ms).max(0) as u64),
                    text,
                })
                .collect(),
            source,
        })
    }

    /// Index of the line being sung at `position_ms` (synced only).
    pub fn current_line(&self, position_ms: u64) -> Option<usize> {
        if !self.synced {
            return None;
        }
        self.lines
            .iter()
            .rposition(|l| l.start_ms.is_some_and(|s| s <= position_ms))
    }
}

/// `mm:ss`, `mm:ss.x`, `mm:ss.xx`, `mm:ss.xxx` (or `:` before the fraction) in ms.
fn parse_stamp(tag: &str) -> Option<i64> {
    let (minutes, rest) = tag.split_once(':')?;
    let minutes: i64 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = match rest.split_once(['.', ':']) {
        Some((s, f)) => (s, f),
        None => (rest, ""),
    };
    let seconds: i64 = seconds.trim().parse().ok()?;
    if !(0..60).contains(&seconds) || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let fraction_ms = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 100,
        2 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction[..3].parse().ok()?,
    };
    Some((minutes * 60 + seconds) * 1000 + fraction_ms)
}

/// Removes `<mm:ss.xx>` word timings from an enhanced-LRC line.
fn strip_word_stamps(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find('<') {
        let Some(len) = rest[start..].find('>') else {
            break;
        };
        if parse_stamp(&rest[start + 1..start + len]).is_none() {
            out.push_str(&rest[..=start]);
            rest = &rest[start + 1..];
            continue;
        }
        out.push_str(&rest[..start]);
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lookup results keyed by track ID and whether online lookup was allowed.
type Cache = HashMap<(String, bool), Option<Rc<Lyrics>>>;

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

fn lrclib() -> Option<&'static Lrclib> {
    static CLIENT: OnceLock<Option<Lrclib>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            // Debug builds can point at a mock server.
            let base = std::env::var("LIO_LRCLIB_URL")
                .ok()
                .filter(|_| cfg!(debug_assertions));
            let client = match base {
                Some(base) => Lrclib::with_base(&base),
                None => Lrclib::new(),
            };
            client
                .inspect_err(|e| tracing::warn!("LRCLIB unavailable: {e}"))
                .ok()
        })
        .as_ref()
}

/// Result of [`load`].
pub struct Lookup {
    pub lyrics: Option<Rc<Lyrics>>,
    /// `false` when a source couldn't be asked (not signed in yet, server or LRCLIB
    /// error, timeout): trying again later may find (better) lyrics.
    pub complete: bool,
}

impl Lookup {
    fn incomplete() -> Self {
        Self {
            lyrics: None,
            complete: false,
        }
    }
}

/// Looks up lyrics for `track` (see the module docs for the order).
pub async fn load(track: &Track) -> Lookup {
    let app = LioApplication::default();
    let online_lookup = app.settings().boolean("online-lyrics");
    let key = (track.id().to_owned(), online_lookup);
    if let Some(lyrics) = CACHE.with_borrow(|c| c.get(&key).cloned()) {
        return Lookup {
            lyrics,
            complete: true,
        };
    }
    let session = app.session();
    if !session.is_online() {
        tracing::debug!(track = track.id(), "lyrics: not online yet");
        return Lookup::incomplete();
    }
    let Some(account) = session.account() else {
        return Lookup::incomplete();
    };
    let structured = app
        .settings()
        .strv("extensions")
        .iter()
        .any(|e| e == "songLyrics");
    let client = account.client.clone();
    let id = track.id().to_owned();
    let artist = track.artist();
    let title = track.title().to_owned();
    let album = track.album().map(str::to_owned);
    let duration_s = u64::try_from(track.duration_s()).ok().filter(|d| *d > 0);
    let lrclib = online_lookup.then(lrclib).flatten();

    let (lyrics, complete) = runtime::run(async move {
        let mut complete = true;
        let server = if structured {
            client
                .get_lyrics_by_song_id(&id)
                .await
                .map(Lyrics::from_structured)
        } else {
            client.get_lyrics(&artist, &title).await.map(|t| {
                t.as_deref()
                    .and_then(|t| Lyrics::from_plain(t, Source::Server))
            })
        };
        let server = server.unwrap_or_else(|e| {
            tracing::debug!("server lyrics lookup failed: {e}");
            complete = false;
            None
        });
        if server.as_ref().is_some_and(|l| l.synced) {
            return (server, complete);
        }
        let Some(lrclib) = lrclib else {
            return (server, complete);
        };
        let query = LyricsQuery {
            artist: &artist,
            title: &title,
            album: album.as_deref(),
            duration_s,
        };
        match lrclib.find(&query).await {
            Ok(Some(record)) if !record.instrumental => {
                let timed = record
                    .synced_lyrics
                    .as_deref()
                    .and_then(|t| Lyrics::from_lrc(t, Source::Lrclib));
                let plain = || {
                    record
                        .plain_lyrics
                        .as_deref()
                        .and_then(|t| Lyrics::from_plain(t, Source::Lrclib))
                };
                (timed.or(server).or_else(plain), complete)
            }
            Ok(_) => (server, complete),
            Err(e) => {
                tracing::debug!("LRCLIB lookup failed: {e}");
                (server, false)
            }
        }
    })
    .await;

    let lyrics = lyrics.map(Rc::new);
    tracing::debug!(
        track = key.0,
        found = lyrics.is_some(),
        complete,
        "lyrics lookup"
    );
    // A failed lookup isn't remembered, so a later attempt can succeed.
    if complete {
        CACHE.with_borrow_mut(|c| {
            if c.len() >= CACHE_LIMIT {
                c.clear();
            }
            c.insert(key, lyrics.clone());
        });
    }
    Lookup { lyrics, complete }
}

#[cfg(test)]
mod tests {
    use lio_subsonic::types::LyricLine;

    use super::*;

    fn set(synced: bool, offset: i64, lines: &[(Option<i64>, &str)]) -> StructuredLyrics {
        StructuredLyrics {
            lang: "xxx".into(),
            synced,
            offset,
            line: lines
                .iter()
                .map(|(start, value)| LyricLine {
                    start: *start,
                    value: (*value).into(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn prefers_synced_and_applies_offset() {
        let lyrics = Lyrics::from_structured(vec![
            set(false, 0, &[(None, "plain")]),
            set(true, -200, &[(Some(100), "one"), (Some(1000), "two")]),
        ])
        .unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.source, Source::Server);
        assert_eq!(lyrics.lines[0].start_ms, Some(0));
        assert_eq!(lyrics.lines[1].start_ms, Some(800));
        assert_eq!(lyrics.current_line(0), Some(0));
        assert_eq!(lyrics.current_line(799), Some(0));
        assert_eq!(lyrics.current_line(800), Some(1));
    }

    #[test]
    fn before_first_line_and_unsynced() {
        let synced = Lyrics::from_structured(vec![set(true, 0, &[(Some(500), "a")])]).unwrap();
        assert_eq!(synced.current_line(100), None);
        let plain = Lyrics::from_plain("a\nb\n\nc\n", Source::Server).unwrap();
        assert!(!plain.synced);
        assert_eq!(plain.lines.len(), 4);
        assert_eq!(plain.lines[2].text, "");
        assert_eq!(plain.current_line(10_000), None);
    }

    #[test]
    fn empty_results() {
        assert!(Lyrics::from_structured(vec![]).is_none());
        assert!(Lyrics::from_structured(vec![set(true, 0, &[])]).is_none());
        assert!(Lyrics::from_plain("  \n", Source::Server).is_none());
        assert!(Lyrics::from_lrc("[ar:Someone]\nno stamps here", Source::Lrclib).is_none());
    }

    #[test]
    fn parses_lrc() {
        let lrc = "[ar:Lio Fixtures]\n[ti:Sweep]\n[length: 00:20]\n\
                   [00:01.20] First line\n\
                   [00:05.5]Second <00:05.50>word <00:06.00>timed\n\
                   [00:09.75][00:15.000] Chorus\n\
                   [00:12.00]\n\
                   not a lyric line\n";
        let lyrics = Lyrics::from_lrc(lrc, Source::Lrclib).unwrap();
        assert!(lyrics.synced);
        let got: Vec<(u64, &str)> = lyrics
            .lines
            .iter()
            .map(|l| (l.start_ms.unwrap(), l.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (1200, "First line"),
                (5500, "Second word timed"),
                (9750, "Chorus"),
                (12000, ""),
                (15000, "Chorus"),
            ]
        );
        assert_eq!(lyrics.current_line(10_000), Some(2));
    }

    #[test]
    fn lrc_offset_and_odd_stamps() {
        // Positive offset: lyrics come earlier.
        let lyrics =
            Lyrics::from_lrc("[offset:+500]\n[00:01.00]a\n[01:02:50]b", Source::Lrclib).unwrap();
        assert_eq!(lyrics.lines[0].start_ms, Some(500));
        assert_eq!(lyrics.lines[1].start_ms, Some(62_000));
        assert_eq!(parse_stamp("3:07"), Some(187_000));
        assert_eq!(parse_stamp("00:61.00"), None);
        assert_eq!(parse_stamp("ar:Name"), None);
        assert_eq!(strip_word_stamps("a <b> c"), "a <b> c");
    }
}
