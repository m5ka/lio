//! Interface between `liosrc` and whatever provides the audio bytes (Lio's TrackStore).

use std::io;
use std::sync::{Arc, OnceLock, RwLock};

/// A random-access byte source for one track.
pub trait TrackSource: Send + Sync {
    /// Total size in bytes, if known. Unknown sizes make the stream non-seekable.
    fn size(&self) -> Option<u64>;

    /// Reads up to `buf.len()` bytes at `offset`, blocking until at least one byte is
    /// available. Returns `Ok(0)` at end of stream, `ErrorKind::Interrupted` after
    /// [`TrackSource::cancel`], and `ErrorKind::TimedOut` when data never arrives.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;

    /// Unblocks a pending [`TrackSource::read_at`] (flush / state change).
    fn cancel(&self);

    /// Clears a previous cancellation (after a flush finishes).
    fn resume(&self);
}

/// Opens [`TrackSource`]s for `lio://` URIs.
pub trait TrackResolver: Send + Sync {
    /// `track_id` and `quality` come from `lio://track/<track_id>?q=<quality>`.
    fn open(&self, track_id: &str, quality: &str) -> Result<Box<dyn TrackSource>, String>;
}

fn slot() -> &'static RwLock<Option<Arc<dyn TrackResolver>>> {
    static RESOLVER: OnceLock<RwLock<Option<Arc<dyn TrackResolver>>>> = OnceLock::new();
    RESOLVER.get_or_init(|| RwLock::new(None))
}

/// Installs the resolver used by every `liosrc` instance.
pub fn set_resolver(resolver: Arc<dyn TrackResolver>) {
    *slot().write().unwrap() = Some(resolver);
}

pub(crate) fn resolver() -> Option<Arc<dyn TrackResolver>> {
    slot().read().unwrap().clone()
}

/// Builds the URI `liosrc` understands.
pub fn track_uri(track_id: &str, quality: &str) -> String {
    let mut url = url::Url::parse("lio://track/").expect("static URL");
    url.path_segments_mut()
        .expect("hierarchical")
        .pop_if_empty()
        .push(track_id);
    url.query_pairs_mut().append_pair("q", quality);
    url.into()
}

/// Like [`track_uri`] with an extra `k` parameter, so the same track queued twice gets
/// distinct URIs (the gapless switch is detected by URI).
pub fn track_uri_keyed(track_id: &str, quality: &str, key: u64) -> String {
    let mut uri = track_uri(track_id, quality);
    uri.push_str(&format!("&k={key}"));
    uri
}

/// Parses `lio://track/<id>?q=<quality>` into `(id, quality)`.
pub fn parse_track_uri(uri: &str) -> Option<(String, String)> {
    let url = url::Url::parse(uri).ok()?;
    if url.scheme() != "lio" || url.host_str() != Some("track") {
        return None;
    }
    let id = url.path_segments()?.next().filter(|s| !s.is_empty())?;
    let id = percent_decode(id);
    let quality = url
        .query_pairs()
        .find(|(k, _)| k == "q")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| "original".into());
    Some((id, quality))
}

fn percent_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={s}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_round_trip() {
        let uri = track_uri("3rKEO/zE S", "opus-128");
        assert!(uri.starts_with("lio://track/"));
        assert_eq!(
            parse_track_uri(&uri),
            Some(("3rKEO/zE S".into(), "opus-128".into()))
        );
        assert_eq!(
            parse_track_uri("lio://track/abc"),
            Some(("abc".into(), "original".into()))
        );
        assert_eq!(parse_track_uri("file:///x"), None);
        assert_eq!(parse_track_uri("lio://track/"), None);
        let keyed = track_uri_keyed("abc", "original", 7);
        assert!(keyed.ends_with("&k=7"));
        assert_eq!(
            parse_track_uri(&keyed),
            Some(("abc".into(), "original".into()))
        );
    }
}
