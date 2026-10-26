//! TrackStore: the single audio data path for downloads, cache and streaming.
//!
//! Resolution order:
//! 1. complete download at any quality,
//! 2. complete cache entry at the requested quality,
//! 3. complete cache entry at another quality (original first, then higher bitrate),
//! 4. fetch at the requested quality into the cache (online only).
//!
//! A fetch writes `<cache>/audio/<rowid>.<quality>.part` plus a `.ranges` sidecar; readers
//! block until their bytes arrive (at most [`READ_TIMEOUT`]). Reads far from the download
//! position restart the download there with an HTTP `Range` request (original quality only).

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use lio_subsonic::Client;
use rusqlite::{OptionalExtension, params};

use crate::db::{Db, now};
use crate::error::{Error, Result};
use crate::model::Quality;
use crate::paths::AccountPaths;
use crate::ranges::Ranges;

/// Longest a reader waits for bytes that haven't arrived.
pub const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// A read this far beyond the download position restarts the download at the read.
const SEEK_RESTART_DISTANCE: u64 = 512 * 1024;
/// Persist the range sidecar after this many new bytes.
const SIDECAR_EVERY: u64 = 1024 * 1024;
const MAX_ATTEMPTS: u32 = 3;

type FetchKey = (i64, Quality);

/// Where an opened track's bytes come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Download(Quality),
    Cache(Quality),
    Stream(Quality),
}

/// Called after a track was added to the cache: `(track_rowid, bytes)`.
pub type CacheWriteHook = Arc<dyn Fn(i64, u64) + Send + Sync>;

#[derive(Clone)]
pub struct TrackStore {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    paths: AccountPaths,
    runtime: tokio::runtime::Handle,
    client: RwLock<Option<Client>>,
    online: AtomicBool,
    fetches: Mutex<HashMap<FetchKey, Arc<Fetch>>>,
    /// Tracks that must not be evicted (current and prefetch target).
    protected: Mutex<HashSet<i64>>,
    on_cache_write: RwLock<Option<CacheWriteHook>>,
    /// Audio cache limit in bytes (`cache-limit-mb`).
    cache_limit: std::sync::atomic::AtomicU64,
}

impl TrackStore {
    pub fn new(db: Db, paths: AccountPaths, runtime: tokio::runtime::Handle) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                paths,
                runtime,
                client: RwLock::new(None),
                online: AtomicBool::new(true),
                fetches: Mutex::new(HashMap::new()),
                protected: Mutex::new(HashSet::new()),
                on_cache_write: RwLock::new(None),
                cache_limit: std::sync::atomic::AtomicU64::new(2 * 1024 * 1024 * 1024),
            }),
        }
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn paths(&self) -> &AccountPaths {
        &self.inner.paths
    }

    pub fn set_client(&self, client: Option<Client>) {
        *self.inner.client.write().unwrap() = client;
    }

    pub fn client(&self) -> Option<Client> {
        self.inner.client.read().unwrap().clone()
    }

    pub fn set_online(&self, online: bool) {
        self.inner.online.store(online, Ordering::SeqCst);
    }

    pub fn is_online(&self) -> bool {
        self.inner.online.load(Ordering::SeqCst)
    }

    pub fn set_cache_limit(&self, bytes: u64) {
        self.inner.cache_limit.store(bytes, Ordering::SeqCst);
    }

    pub fn cache_limit(&self) -> u64 {
        self.inner.cache_limit.load(Ordering::SeqCst)
    }

    /// `.part` files owned by running fetches (never evicted).
    fn active_parts(&self) -> HashSet<PathBuf> {
        self.inner
            .fetches
            .lock()
            .unwrap()
            .values()
            .map(|f| f.part_path.clone())
            .collect()
    }

    /// Evicts least-used cache entries until the cache fits its limit.
    pub fn enforce_cache_limit(&self) -> Result<u64> {
        let (limit, protected, active) =
            (self.cache_limit(), self.protected(), self.active_parts());
        let dir = self.inner.paths.audio_cache();
        self.inner
            .db
            .with(|c| crate::cache::enforce_limit(c, &dir, limit, &protected, &active, now()))
    }

    /// Removes all cached audio except the current and next track.
    pub fn clear_cache(&self) -> Result<u64> {
        let (protected, active) = (self.protected(), self.active_parts());
        let dir = self.inner.paths.audio_cache();
        self.inner
            .db
            .with(|c| crate::cache::clear(c, &dir, &protected, &active))
    }

    pub fn usage(&self) -> Result<crate::cache::Usage> {
        let dir = self.inner.paths.audio_cache();
        self.inner.db.with(|c| crate::cache::usage(c, &dir))
    }

    pub fn set_cache_write_hook(&self, hook: Option<CacheWriteHook>) {
        *self.inner.on_cache_write.write().unwrap() = hook;
    }

    /// Replaces the set of tracks protected from eviction.
    pub fn set_protected(&self, rowids: impl IntoIterator<Item = i64>) {
        *self.inner.protected.lock().unwrap() = rowids.into_iter().collect();
    }

    pub fn protected(&self) -> HashSet<i64> {
        self.inner.protected.lock().unwrap().clone()
    }

    /// Opens a track by server ID. Blocking (database and file I/O); call from a worker
    /// or streaming thread, never the UI thread.
    pub fn open_by_id(&self, track_id: &str, requested: Quality) -> Result<TrackHandle> {
        let rowid: i64 = self
            .inner
            .db
            .with(|c| {
                Ok(
                    c.query_row("SELECT rowid FROM track WHERE id = ?1", [track_id], |r| {
                        r.get(0)
                    })
                    .optional()?,
                )
            })?
            .ok_or_else(|| Error::Other(format!("unknown track {track_id}")))?;
        self.open(rowid, requested)
    }

    /// Opens a track by rowid following the resolution order. Blocking.
    pub fn open(&self, rowid: i64, requested: Quality) -> Result<TrackHandle> {
        if let Some((origin, path)) = self.resolve_local(rowid, requested)? {
            match File::open(&path) {
                Ok(file) => {
                    let size = file.metadata()?.len();
                    if let Origin::Cache(q) = origin {
                        self.touch_cache(rowid, q)?;
                    }
                    tracing::debug!(rowid, ?origin, "opened local copy");
                    return Ok(TrackHandle::file(file, size, origin));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    tracing::warn!(rowid, ?path, "local copy missing on disk; forgetting it");
                    self.forget_local(rowid, origin)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        let fetch = self.fetch(rowid, requested)?;
        TrackHandle::stream(fetch, requested)
    }

    /// Where a local copy of `rowid` lives, without opening it.
    pub fn resolve_local(
        &self,
        rowid: i64,
        requested: Quality,
    ) -> Result<Option<(Origin, PathBuf)>> {
        self.inner.db.with(|c| {
            let download: Option<(String, String)> = c
                .query_row(
                    "SELECT quality, path FROM download_file
                     WHERE track_rowid = ?1 AND complete ORDER BY quality = ?2 DESC LIMIT 1",
                    params![rowid, requested.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((q, path)) = download {
                let q = Quality::parse(&q).unwrap_or_default();
                return Ok(Some((Origin::Download(q), path.into())));
            }
            let mut stmt = c.prepare_cached(
                "SELECT quality, path FROM cache_entry WHERE track_rowid = ?1 AND complete",
            )?;
            let mut cached: Vec<(Quality, PathBuf)> = stmt
                .query_map([rowid], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .filter_map(|r| r.ok())
                .filter_map(|(q, p)| Some((Quality::parse(&q)?, PathBuf::from(p))))
                .collect();
            cached.sort_by_key(|(q, _)| (*q != requested, q.preference()));
            Ok(cached
                .into_iter()
                .next()
                .map(|(q, p)| (Origin::Cache(q), p)))
        })
    }

    /// Whether a complete local copy exists.
    pub fn is_available_offline(&self, rowid: i64) -> Result<bool> {
        Ok(self.resolve_local(rowid, Quality::Original)?.is_some())
    }

    /// Starts fetching `rowid` into the cache in the background unless a local copy
    /// exists or a fetch is already running. Blocking (database lookup).
    pub fn prefetch(&self, rowid: i64, quality: Quality) -> Result<()> {
        if self.resolve_local(rowid, quality)?.is_some() {
            return Ok(());
        }
        self.fetch(rowid, quality).map(drop)
    }

    /// Fetches in progress, for diagnostics and tests.
    pub fn active_fetches(&self) -> usize {
        self.inner.fetches.lock().unwrap().len()
    }

    /// Cancels every running fetch (sign-out, going offline).
    pub fn cancel_all(&self) {
        let fetches: Vec<_> = self
            .inner
            .fetches
            .lock()
            .unwrap()
            .drain()
            .map(|(_, f)| f)
            .collect();
        for fetch in fetches {
            fetch.abort();
        }
    }

    fn touch_cache(&self, rowid: i64, quality: Quality) -> Result<()> {
        self.inner.db.with(|c| {
            c.execute(
                "UPDATE cache_entry SET last_played_at = ?1, play_count = play_count + 1
                 WHERE track_rowid = ?2 AND quality = ?3",
                params![now(), rowid, quality.as_str()],
            )?;
            Ok(())
        })
    }

    fn forget_local(&self, rowid: i64, origin: Origin) -> Result<()> {
        self.inner.db.with(|c| {
            match origin {
                Origin::Download(q) => c.execute(
                    "UPDATE download_file SET complete = 0 WHERE track_rowid = ?1 AND quality = ?2",
                    params![rowid, q.as_str()],
                )?,
                Origin::Cache(q) => c.execute(
                    "DELETE FROM cache_entry WHERE track_rowid = ?1 AND quality = ?2",
                    params![rowid, q.as_str()],
                )?,
                Origin::Stream(_) => 0,
            };
            Ok(())
        })
    }

    /// Returns the running fetch for `(rowid, quality)` or starts one.
    fn fetch(&self, rowid: i64, quality: Quality) -> Result<Arc<Fetch>> {
        let key = (rowid, quality);
        if let Some(existing) = self.inner.fetches.lock().unwrap().get(&key) {
            return Ok(existing.clone());
        }
        if !self.is_online() {
            return Err(Error::UnavailableOffline);
        }
        let client = self.client().ok_or(Error::UnavailableOffline)?;
        let track_id: String = self.inner.db.with(|c| {
            Ok(
                c.query_row("SELECT id FROM track WHERE rowid = ?1", [rowid], |r| {
                    r.get(0)
                })?,
            )
        })?;

        let dir = self.inner.paths.audio_cache();
        std::fs::create_dir_all(&dir)?;
        let final_path = dir.join(format!("{rowid}.{quality}"));
        let part_path = dir.join(format!("{rowid}.{quality}.part"));
        // Transcodes aren't byte-identical between runs, so only originals resume.
        let resume = !quality.is_transcoded();
        let ranges = if resume {
            Ranges::load(&Ranges::sidecar_path(&part_path))
        } else {
            Ranges::new()
        };
        if !ranges.is_empty() {
            tracing::debug!(
                rowid,
                bytes = ranges.total_bytes(),
                "resuming partial download"
            );
        }
        let writer = OpenOptions::new()
            .create(true)
            .truncate(!resume)
            .write(true)
            .read(true)
            .open(&part_path)?;

        let fetch = Arc::new(Fetch {
            key,
            part_path,
            final_path,
            writer,
            state: Mutex::new(FetchState {
                ranges,
                ..FetchState::default()
            }),
            cond: Condvar::new(),
        });

        let mut fetches = self.inner.fetches.lock().unwrap();
        if let Some(existing) = fetches.get(&key) {
            return Ok(existing.clone());
        }
        fetches.insert(key, fetch.clone());
        drop(fetches);

        let store = self.clone();
        let task_fetch = fetch.clone();
        self.inner.runtime.spawn(async move {
            let result = run_fetch(&client, &track_id, &task_fetch).await;
            store.finish_fetch(&task_fetch, result);
        });
        Ok(fetch)
    }

    fn finish_fetch(&self, fetch: &Arc<Fetch>, result: Result<u64>) {
        let (rowid, quality) = fetch.key;
        let outcome = result.and_then(|size| {
            fetch.writer.sync_all()?;
            std::fs::rename(&fetch.part_path, &fetch.final_path)?;
            let _ = std::fs::remove_file(Ranges::sidecar_path(&fetch.part_path));
            let path = fetch.final_path.to_string_lossy().into_owned();
            self.inner.db.with(|c| {
                let t = now();
                c.execute(
                    "INSERT INTO cache_entry (track_rowid, quality, path, size_bytes, complete,
                                              last_played_at, play_count, created_at)
                     VALUES (?1, ?2, ?3, ?4, 1, ?5, 1, ?5)
                     ON CONFLICT (track_rowid, quality) DO UPDATE SET
                        path = excluded.path, size_bytes = excluded.size_bytes, complete = 1",
                    params![rowid, quality.as_str(), path, size as i64, t],
                )?;
                Ok(())
            })?;
            Ok(size)
        });
        let mut state = fetch.state.lock().unwrap();
        match outcome {
            Ok(size) => {
                tracing::debug!(rowid, %quality, size, "cached track");
                state.total = Some(size);
                state.exact_total = true;
                state.done = true;
                drop(state);
                fetch.cond.notify_all();
                if let Err(e) = self.enforce_cache_limit() {
                    tracing::warn!("cache eviction failed: {e}");
                }
                if let Some(hook) = self.inner.on_cache_write.read().unwrap().clone() {
                    hook(rowid, size);
                }
            }
            Err(e) => {
                tracing::warn!(rowid, %quality, "fetch failed: {e}");
                state.failed = Some(e.to_string());
                state.disk_full = matches!(e, Error::DiskFull);
                drop(state);
                fetch.cond.notify_all();
                let _ = fetch.writer.sync_all();
                let ranges = fetch.state.lock().unwrap().ranges.clone();
                let _ = ranges.save(&Ranges::sidecar_path(&fetch.part_path));
            }
        }
        // Only now is the result visible (cache row written), so callers waiting for
        // `active_fetches() == 0` see a consistent state.
        self.inner.fetches.lock().unwrap().remove(&fetch.key);
    }
}

#[derive(Default)]
struct FetchState {
    ranges: Ranges,
    /// Size of the resource (an estimate for transcodes until `exact_total`).
    total: Option<u64>,
    exact_total: bool,
    done: bool,
    failed: Option<String>,
    disk_full: bool,
    aborted: bool,
    /// Offset a blocked reader is waiting for.
    want: Option<u64>,
}

struct Fetch {
    key: FetchKey,
    part_path: PathBuf,
    final_path: PathBuf,
    writer: File,
    state: Mutex<FetchState>,
    cond: Condvar,
}

impl Fetch {
    fn abort(&self) {
        self.state.lock().unwrap().aborted = true;
        self.cond.notify_all();
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> Result<()> {
        self.writer
            .write_all_at(data, offset)
            .map_err(Error::from_io)?;
        let mut state = self.state.lock().unwrap();
        state.ranges.insert(offset, offset + data.len() as u64);
        drop(state);
        self.cond.notify_all();
        Ok(())
    }
}

/// Downloads until the whole file is present; returns its size.
async fn run_fetch(client: &Client, track_id: &str, fetch: &Fetch) -> Result<u64> {
    let quality = fetch.key.1;
    let (format, max_bit_rate) = quality.stream_params();
    let url = client.stream_url(track_id, format, max_bit_rate, None);
    let mut attempts = 0;
    let mut cursor = 0;

    loop {
        let start = {
            let mut st = fetch.state.lock().unwrap();
            if st.aborted {
                return Err(Error::Cancelled);
            }
            if let Some(total) = st.total.filter(|_| st.exact_total)
                && st.ranges.covers(total)
            {
                return Ok(total);
            }
            let wanted = st.want.take().filter(|w| !st.ranges.contains(*w, *w + 1));
            let total = st.total.unwrap_or(u64::MAX);
            match wanted {
                Some(w) => w,
                None => st.ranges.next_missing(cursor, total).map_or(0, |(s, _)| s),
            }
        };
        // Transcoded streams don't support ranges: always (re)start from 0.
        let start = if quality.is_transcoded() { 0 } else { start };

        match stream_from(client, &url, start, fetch).await {
            Ok(StreamEnd::Complete { end, full_response }) => {
                attempts = 0;
                cursor = end;
                if full_response {
                    // A full 200 response defines the exact size (fixes transcode estimates).
                    let mut st = fetch.state.lock().unwrap();
                    st.total = Some(end);
                    st.exact_total = true;
                }
            }
            Ok(StreamEnd::Restart { at }) => {
                cursor = at;
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(Error::DiskFull) => return Err(Error::DiskFull),
            Err(e) => {
                attempts += 1;
                if attempts >= MAX_ATTEMPTS {
                    return Err(e);
                }
                tracing::debug!(attempts, "stream interrupted, retrying: {e}");
                tokio::time::sleep(Duration::from_millis(500 * u64::from(attempts))).await;
            }
        }
    }
}

enum StreamEnd {
    /// The response body ended at byte `end`.
    Complete { end: u64, full_response: bool },
    /// A reader wants bytes far away; restart at `at`.
    Restart { at: u64 },
}

async fn stream_from(
    client: &Client,
    url: &url::Url,
    start: u64,
    fetch: &Fetch,
) -> Result<StreamEnd> {
    let quality = fetch.key.1;
    let response = client.open_stream(url.clone(), start).await?;
    let full_response = !response.partial;
    let mut pos = response.start;
    {
        let mut st = fetch.state.lock().unwrap();
        if let Some(total) = response.total {
            if response.partial || !quality.is_transcoded() {
                st.total = Some(total);
                st.exact_total = true;
            } else if !st.exact_total {
                st.total = Some(total);
            }
        }
    }

    let mut body = response.response.bytes_stream();
    let mut since_sidecar = 0u64;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| client.map_transport_error(e))?;
        fetch.write_at(pos, &chunk)?;
        pos += chunk.len() as u64;
        since_sidecar += chunk.len() as u64;
        if since_sidecar >= SIDECAR_EVERY {
            since_sidecar = 0;
            let ranges = fetch.state.lock().unwrap().ranges.clone();
            let _ = ranges.save(&Ranges::sidecar_path(&fetch.part_path));
        }

        let st = fetch.state.lock().unwrap();
        if st.aborted {
            return Err(Error::Cancelled);
        }
        if !quality.is_transcoded()
            && let Some(want) = st.want
            && !st.ranges.contains(want, want + 1)
            && (want < pos || want > pos + SEEK_RESTART_DISTANCE)
        {
            return Ok(StreamEnd::Restart { at: want });
        }
        // Already have what follows (resumed download): jump to the next gap.
        if !quality.is_transcoded() && st.ranges.available_from(pos) > 0 {
            let next = st.ranges.first_missing(pos, st.total.unwrap_or(u64::MAX));
            drop(st);
            return Ok(match next {
                Some((gap, _)) => StreamEnd::Restart { at: gap },
                None => StreamEnd::Complete {
                    end: pos,
                    full_response: false,
                },
            });
        }
    }
    Ok(StreamEnd::Complete {
        end: pos,
        full_response,
    })
}

/// Readable handle to one track, local or streaming.
pub struct TrackHandle {
    kind: HandleKind,
    origin: Origin,
    cancelled: AtomicBool,
}

enum HandleKind {
    File { file: File, size: u64 },
    Stream { fetch: Arc<Fetch>, file: File },
}

impl TrackHandle {
    fn file(file: File, size: u64, origin: Origin) -> Self {
        Self {
            kind: HandleKind::File { file, size },
            origin,
            cancelled: AtomicBool::new(false),
        }
    }

    fn stream(fetch: Arc<Fetch>, quality: Quality) -> Result<Self> {
        // The fetch may have just completed and renamed its file.
        let file = match File::open(&fetch.part_path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => File::open(&fetch.final_path)?,
            other => other?,
        };
        Ok(Self {
            kind: HandleKind::Stream { fetch, file },
            origin: Origin::Stream(quality),
            cancelled: AtomicBool::new(false),
        })
    }

    pub fn origin(&self) -> Origin {
        self.origin
    }

    /// Size in bytes if exact. Transcoded streams report `None` until complete,
    /// which makes them non-seekable while streaming.
    pub fn size(&self) -> Option<u64> {
        match &self.kind {
            HandleKind::File { size, .. } => Some(*size),
            HandleKind::Stream { fetch, .. } => {
                let quality = fetch.key.1;
                let deadline = Instant::now() + READ_TIMEOUT;
                let mut st = fetch.state.lock().unwrap();
                // Wait briefly for the response headers so the size is known.
                while st.total.is_none() && st.failed.is_none() && !st.done && !st.aborted {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() || self.cancelled.load(Ordering::SeqCst) {
                        break;
                    }
                    st = fetch
                        .cond
                        .wait_timeout(st, left.min(Duration::from_millis(100)))
                        .unwrap()
                        .0;
                }
                if quality.is_transcoded() && !st.exact_total {
                    None
                } else {
                    st.total
                }
            }
        }
    }

    /// Reads at `offset`, blocking until bytes arrive (see [`READ_TIMEOUT`]).
    /// `Ok(0)` means end of stream.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &self.kind {
            HandleKind::File { file, size } => {
                if offset >= *size {
                    return Ok(0);
                }
                let n = buf.len().min((*size - offset) as usize);
                file.read_at(&mut buf[..n], offset)
            }
            HandleKind::Stream { fetch, file } => {
                // Wait for the whole request: demuxers reading in pull mode (id3demux,
                // typefind) treat a short read as the end of the file.
                let mut deadline = Instant::now() + READ_TIMEOUT;
                let mut progress = 0;
                let mut st = fetch.state.lock().unwrap();
                loop {
                    if self.cancelled.load(Ordering::SeqCst) {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    let end_known = st.exact_total || st.done;
                    let need = match st.total.filter(|_| end_known) {
                        Some(total) if offset >= total => return Ok(0),
                        Some(total) => buf.len().min((total - offset) as usize),
                        None => buf.len(),
                    };
                    let avail = st.ranges.available_from(offset).min(need as u64) as usize;
                    // A failed or finished fetch won't bring more: hand out what's there.
                    let finished = st.done || st.failed.is_some() || st.aborted;
                    if avail == need || (avail > 0 && finished) {
                        drop(st);
                        return file.read_at(&mut buf[..avail], offset);
                    }
                    if st.done {
                        return Ok(0);
                    }
                    if let Some(msg) = &st.failed {
                        let kind = if st.disk_full {
                            io::ErrorKind::StorageFull
                        } else {
                            io::ErrorKind::Other
                        };
                        return Err(io::Error::new(kind, msg.clone()));
                    }
                    if st.aborted {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    if avail > progress {
                        // Still arriving: the timeout counts from the last progress.
                        progress = avail;
                        deadline = Instant::now() + READ_TIMEOUT;
                    }
                    st.want = Some(offset + avail as u64);
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        if avail > 0 {
                            drop(st);
                            return file.read_at(&mut buf[..avail], offset);
                        }
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            Error::Timeout.to_string(),
                        ));
                    }
                    st = fetch
                        .cond
                        .wait_timeout(st, left.min(Duration::from_millis(250)))
                        .unwrap()
                        .0;
                }
            }
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let HandleKind::Stream { fetch, .. } = &self.kind {
            fetch.cond.notify_all();
        }
    }

    pub fn resume(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }
}
