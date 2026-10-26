//! Pinned downloads.
//!
//! A pin (album, playlist or Favourites) resolves to a set of tracks (`download_track`).
//! Files live in `download_file`, shared between pins: a file is deleted only when no
//! pin references it. A single worker fetches missing files at the pin's quality,
//! resuming partial files, and moves pins through the status machine:
//!
//! `Queued → Downloading → Complete`, `Downloading ⇄ Paused`,
//! `Downloading → Waiting (offline / metered) → Downloading`,
//! `Downloading → Error (disk full | server) → (retry) Downloading`,
//! `Complete → Syncing → Complete`, any → removed.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use lio_subsonic::Client;
use rusqlite::{Connection, OptionalExtension, params};

use crate::db::{Db, now};
use crate::error::{Error, Result};
use crate::model::Quality;
use crate::paths::AccountPaths;
use crate::queries;

/// Free space kept in reserve beyond the file being downloaded.
const DISK_RESERVE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PinKind {
    Album,
    Playlist,
    Favourites,
}

impl PinKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Playlist => "playlist",
            Self::Favourites => "favourites",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "album" => Some(Self::Album),
            "playlist" => Some(Self::Playlist),
            "favourites" => Some(Self::Favourites),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Queued,
    Downloading,
    Paused,
    /// Offline, or metered without permission.
    Waiting,
    Complete,
    Syncing,
    Error(String),
}

impl Status {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Waiting => "waiting",
            Self::Complete => "complete",
            Self::Syncing => "syncing",
            Self::Error(_) => "error",
        }
    }

    fn from_db(status: &str, error: Option<String>) -> Self {
        match status {
            "downloading" => Self::Downloading,
            "paused" => Self::Paused,
            "waiting" => Self::Waiting,
            "complete" => Self::Complete,
            "syncing" => Self::Syncing,
            "error" => Self::Error(error.unwrap_or_default()),
            _ => Self::Queued,
        }
    }

    /// Whether the worker should fetch files for a pin in this status.
    pub fn wants_work(&self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Downloading | Self::Syncing | Self::Waiting
        )
    }
}

pub const DISK_FULL: &str = "disk full";

#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    pub rowid: i64,
    pub kind: PinKind,
    pub ref_id: String,
    pub quality: Quality,
    pub status: Status,
    pub created_at: i64,
    pub last_synced_at: Option<i64>,
    pub total_tracks: i64,
    pub done_tracks: i64,
    pub bytes: i64,
}

impl Pin {
    /// "Available offline" only once complete (nothing partial is shown).
    pub fn available_offline(&self) -> bool {
        self.status == Status::Complete
    }
}

// Database ------------------------------------------------------------------------------

const PIN_COLUMNS: &str = "d.rowid, d.kind, d.ref_id, d.quality, d.status, d.error, d.created_at, d.last_synced_at,
     (SELECT COUNT(*) FROM download_track t WHERE t.download_rowid = d.rowid),
     (SELECT COUNT(*) FROM download_track t WHERE t.download_rowid = d.rowid
        AND EXISTS (SELECT 1 FROM download_file f WHERE f.track_rowid = t.track_rowid AND f.complete)),
     (SELECT COALESCE(SUM(f.size_bytes), 0) FROM download_file f
        WHERE f.complete AND f.track_rowid IN (SELECT track_rowid FROM download_track t WHERE t.download_rowid = d.rowid))";

fn pin_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Pin> {
    let kind: String = r.get(1)?;
    let quality: String = r.get(3)?;
    let status: String = r.get(4)?;
    Ok(Pin {
        rowid: r.get(0)?,
        kind: PinKind::parse(&kind).unwrap_or(PinKind::Album),
        ref_id: r.get(2)?,
        quality: Quality::parse(&quality).unwrap_or_default(),
        status: Status::from_db(&status, r.get(5)?),
        created_at: r.get(6)?,
        last_synced_at: r.get(7)?,
        total_tracks: r.get(8)?,
        done_tracks: r.get(9)?,
        bytes: r.get(10)?,
    })
}

pub fn pins(conn: &Connection) -> Result<Vec<Pin>> {
    let sql = format!("SELECT {PIN_COLUMNS} FROM download d ORDER BY d.created_at, d.rowid");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt
        .query_map([], pin_from_row)?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

pub fn pin_row(conn: &Connection, rowid: i64) -> Result<Option<Pin>> {
    let sql = format!("SELECT {PIN_COLUMNS} FROM download d WHERE d.rowid = ?1");
    Ok(conn.query_row(&sql, [rowid], pin_from_row).optional()?)
}

pub fn find_pin(conn: &Connection, kind: PinKind, ref_id: &str) -> Result<Option<Pin>> {
    let sql = format!("SELECT {PIN_COLUMNS} FROM download d WHERE d.kind = ?1 AND d.ref_id = ?2");
    Ok(conn
        .query_row(&sql, params![kind.as_str(), ref_id], pin_from_row)
        .optional()?)
}

pub fn set_status(conn: &Connection, rowid: i64, status: &Status) -> Result<()> {
    let error = match status {
        Status::Error(e) => Some(e.as_str()),
        _ => None,
    };
    conn.execute(
        "UPDATE download SET status = ?1, error = ?2 WHERE rowid = ?3",
        params![status.as_str(), error, rowid],
    )?;
    Ok(())
}

/// Creates (or re-queues) a pin; returns its rowid. Tracks are resolved separately.
pub fn pin(conn: &Connection, kind: PinKind, ref_id: &str, quality: Quality) -> Result<i64> {
    Ok(conn.query_row(
        "INSERT INTO download (kind, ref_id, quality, status, created_at) VALUES (?1, ?2, ?3, 'queued', ?4)
         ON CONFLICT (kind, ref_id) DO UPDATE SET status = 'queued', error = NULL
         RETURNING rowid",
        params![kind.as_str(), ref_id, quality.as_str(), now()],
        |r| r.get(0),
    )?)
}

/// Track rowids a pin should hold, from the local mirror.
fn wanted_tracks(conn: &Connection, kind: PinKind, ref_id: &str) -> Result<Vec<i64>> {
    Ok(match kind {
        PinKind::Album => match queries::album_row_by_id(conn, ref_id)? {
            Some(a) => queries::album_track_rows(conn, a.rowid)?
                .into_iter()
                .map(|t| t.rowid)
                .collect(),
            None => Vec::new(),
        },
        PinKind::Playlist => {
            let rowid: Option<i64> = conn
                .query_row("SELECT rowid FROM playlist WHERE id = ?1", [ref_id], |r| {
                    r.get(0)
                })
                .optional()?;
            match rowid {
                Some(r) => queries::playlist_track_ids(conn, r)?,
                None => Vec::new(),
            }
        }
        PinKind::Favourites => queries::starred_track_ids(conn)?,
    })
}

/// Re-resolves a pin's tracks from the mirror. Returns `(added, removed)` counts and
/// moves a complete pin to `Syncing` when it gained tracks.
pub fn resolve(conn: &mut Connection, rowid: i64) -> Result<(usize, usize)> {
    let Some(pin) = pin_row(conn, rowid)? else {
        return Ok((0, 0));
    };
    let mut wanted = wanted_tracks(conn, pin.kind, &pin.ref_id)?;
    wanted.sort_unstable();
    wanted.dedup();
    let tx = conn.transaction()?;
    let current: Vec<i64> = {
        let mut stmt =
            tx.prepare("SELECT track_rowid FROM download_track WHERE download_rowid = ?1")?;
        stmt.query_map([rowid], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    let removed: Vec<i64> = current
        .iter()
        .copied()
        .filter(|t| wanted.binary_search(t).is_err())
        .collect();
    let added: Vec<i64> = wanted
        .iter()
        .copied()
        .filter(|t| !current.contains(t))
        .collect();
    for t in &removed {
        tx.execute(
            "DELETE FROM download_track WHERE download_rowid = ?1 AND track_rowid = ?2",
            params![rowid, t],
        )?;
    }
    for t in &added {
        tx.execute(
            "INSERT OR IGNORE INTO download_track (download_rowid, track_rowid) VALUES (?1, ?2)",
            params![rowid, t],
        )?;
    }
    if !added.is_empty() && pin.status == Status::Complete {
        tx.execute(
            "UPDATE download SET status = 'syncing' WHERE rowid = ?1",
            [rowid],
        )?;
    }
    tx.commit()?;
    Ok((added.len(), removed.len()))
}

/// Tracks of a pin without a complete file: `(track_rowid, track_id, size_bytes)`.
pub fn missing(conn: &Connection, rowid: i64) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.rowid, t.id, t.size_bytes FROM download_track d JOIN track t ON t.rowid = d.track_rowid
         WHERE d.download_rowid = ?1
           AND NOT EXISTS (SELECT 1 FROM download_file f WHERE f.track_rowid = t.rowid AND f.complete)
         ORDER BY t.album_rowid, t.disc_no, t.track_no",
    )?;
    let rows = stmt
        .query_map([rowid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Deletes files no pin references any more. Returns their paths (already removed).
pub fn collect_garbage(conn: &Connection) -> Result<Vec<PathBuf>> {
    let orphans: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT track_rowid, quality, path FROM download_file
             WHERE track_rowid NOT IN (SELECT track_rowid FROM download_track)",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?
    };
    let mut removed = Vec::new();
    for (rowid, quality, path) in orphans {
        conn.execute(
            "DELETE FROM download_file WHERE track_rowid = ?1 AND quality = ?2",
            params![rowid, quality],
        )?;
        let path = PathBuf::from(path);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(part_path(&path));
        removed.push(path);
    }
    Ok(removed)
}

/// Removes a pin and the files only it referenced.
pub fn remove(conn: &Connection, rowid: i64) -> Result<()> {
    conn.execute("DELETE FROM download WHERE rowid = ?1", [rowid])?;
    collect_garbage(conn)?;
    Ok(())
}

pub fn remove_all(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM download", [])?;
    collect_garbage(conn)?;
    Ok(())
}

/// Offline state of an album: how many of its tracks have a complete local copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfflineState {
    None,
    Partial,
    Complete,
}

pub fn album_offline_state(conn: &Connection, album_rowid: i64) -> Result<OfflineState> {
    let (total, local): (i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                SUM(EXISTS (SELECT 1 FROM download_file f WHERE f.track_rowid = track.rowid AND f.complete)
                    OR EXISTS (SELECT 1 FROM cache_entry c WHERE c.track_rowid = track.rowid AND c.complete))
         FROM track WHERE album_rowid = ?1",
        [album_rowid],
        |r| Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
    )?;
    Ok(match (total, local) {
        (0, _) | (_, 0) => OfflineState::None,
        (t, l) if l >= t => OfflineState::Complete,
        _ => OfflineState::Partial,
    })
}

fn part_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// Free bytes on the filesystem holding `dir`.
pub fn free_space(dir: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // statvfs is a plain query; the only unsafe part is the FFI call itself.
    #[allow(unsafe_code)]
    let ok = unsafe { libc::statvfs(c.as_ptr(), stat.as_mut_ptr()) } == 0;
    if !ok {
        return None;
    }
    #[allow(unsafe_code)]
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::unnecessary_cast)] // fsblkcnt_t / c_ulong widths differ by platform.
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

// Worker --------------------------------------------------------------------------------

/// What the worker may do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub online: bool,
    pub metered: bool,
    pub allow_metered: bool,
}

impl Policy {
    pub fn allowed(&self) -> bool {
        self.online && (!self.metered || self.allow_metered)
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            online: true,
            metered: false,
            allow_metered: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A pin's status or progress changed.
    Changed(i64),
    /// Tracks of a pin disappeared from the server.
    TracksRemoved {
        pin: i64,
        count: usize,
    },
    DiskFull,
}

/// Runs downloads in the background. Cheap to clone.
#[derive(Clone)]
pub struct Downloader {
    inner: Arc<DownloaderInner>,
}

struct DownloaderInner {
    db: Db,
    paths: AccountPaths,
    client: Mutex<Option<Client>>,
    policy: Mutex<Policy>,
    wake: tokio::sync::Notify,
    events: async_channel::Sender<Event>,
    stopped: std::sync::atomic::AtomicBool,
}

impl Downloader {
    /// Creates the worker; spawn [`Downloader::run`] on Tokio.
    pub fn new(db: Db, paths: AccountPaths) -> (Self, async_channel::Receiver<Event>) {
        let (tx, rx) = async_channel::unbounded();
        let inner = DownloaderInner {
            db,
            paths,
            client: Mutex::new(None),
            policy: Mutex::new(Policy::default()),
            wake: tokio::sync::Notify::new(),
            events: tx,
            stopped: std::sync::atomic::AtomicBool::new(false),
        };
        (
            Self {
                inner: Arc::new(inner),
            },
            rx,
        )
    }

    pub fn set_client(&self, client: Option<Client>) {
        *self.inner.client.lock().unwrap() = client;
        self.wake();
    }

    pub fn set_policy(&self, policy: Policy) {
        *self.inner.policy.lock().unwrap() = policy;
        self.wake();
    }

    pub fn wake(&self) {
        self.inner.wake.notify_one();
    }

    pub fn stop(&self) {
        self.inner
            .stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.wake();
    }

    fn emit(&self, event: Event) {
        let _ = self.inner.events.try_send(event);
    }

    /// Pins `(kind, ref_id)`: resolves its tracks and queues the download.
    pub async fn add(&self, kind: PinKind, ref_id: &str, quality: Quality) -> Result<i64> {
        let ref_id = ref_id.to_owned();
        let rowid = self
            .inner
            .db
            .run(move |c| {
                let rowid = pin(c, kind, &ref_id, quality)?;
                resolve(c, rowid)?;
                Ok(rowid)
            })
            .await?;
        self.emit(Event::Changed(rowid));
        self.wake();
        Ok(rowid)
    }

    pub async fn pause(&self, rowid: i64) -> Result<()> {
        self.inner
            .db
            .run(move |c| set_status(c, rowid, &Status::Paused))
            .await?;
        self.emit(Event::Changed(rowid));
        Ok(())
    }

    /// Resumes a paused or failed pin.
    pub async fn resume(&self, rowid: i64) -> Result<()> {
        self.inner
            .db
            .run(move |c| set_status(c, rowid, &Status::Queued))
            .await?;
        self.emit(Event::Changed(rowid));
        self.wake();
        Ok(())
    }

    pub async fn remove(&self, rowid: i64) -> Result<()> {
        self.inner.db.run(move |c| remove(c, rowid)).await?;
        self.emit(Event::Changed(rowid));
        Ok(())
    }

    pub async fn remove_all(&self) -> Result<()> {
        self.inner.db.run(|c| remove_all(c)).await?;
        self.emit(Event::Changed(0));
        Ok(())
    }

    /// Re-resolves every playlist and Favourites pin (and albums, which may have lost
    /// tracks) after a library sync.
    pub async fn sync_pins(&self) -> Result<()> {
        let pins = self.inner.db.run(|c| pins(c)).await?;
        for p in pins {
            let rowid = p.rowid;
            let (added, removed) = self.inner.db.run(move |c| resolve(c, rowid)).await?;
            if removed > 0 {
                self.inner.db.run(|c| collect_garbage(c).map(drop)).await?;
                self.emit(Event::TracksRemoved {
                    pin: rowid,
                    count: removed,
                });
            }
            if added > 0 || removed > 0 {
                self.emit(Event::Changed(rowid));
            }
        }
        self.wake();
        Ok(())
    }

    /// The worker loop. Returns when [`Downloader::stop`] is called.
    pub async fn run(self) {
        loop {
            if self.inner.stopped.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            match self.step().await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => tracing::warn!("download worker: {e}"),
            }
            // Nothing to do (or not allowed): sleep until woken, re-checking now and then.
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(60),
                self.inner.wake.notified(),
            )
            .await;
        }
    }

    /// Processes the next pin that wants work. Returns whether it did anything.
    async fn step(&self) -> Result<bool> {
        let pins = self.inner.db.run(|c| pins(c)).await?;
        let Some(pin) = pins.into_iter().find(|p| p.status.wants_work()) else {
            return Ok(false);
        };
        let policy = *self.inner.policy.lock().unwrap();
        let client = self.inner.client.lock().unwrap().clone();
        let (true, Some(client)) = (policy.allowed(), client) else {
            if pin.status != Status::Waiting {
                let rowid = pin.rowid;
                self.inner
                    .db
                    .run(move |c| set_status(c, rowid, &Status::Waiting))
                    .await?;
                self.emit(Event::Changed(rowid));
            }
            return Ok(false);
        };

        let rowid = pin.rowid;
        let syncing = pin.status == Status::Syncing;
        let working = if syncing {
            Status::Syncing
        } else {
            Status::Downloading
        };
        self.inner
            .db
            .run(move |c| set_status(c, rowid, &working))
            .await?;
        self.emit(Event::Changed(rowid));

        let missing = self.inner.db.run(move |c| missing(c, rowid)).await?;
        for (track_rowid, track_id, size) in missing {
            // Paused, removed or policy changed meanwhile?
            let current = self.inner.db.run(move |c| pin_row(c, rowid)).await?;
            match current {
                Some(p) if p.status.wants_work() => {}
                _ => return Ok(true),
            }
            if !self.inner.policy.lock().unwrap().allowed() {
                return Ok(true);
            }
            let dir = self.inner.paths.downloads();
            std::fs::create_dir_all(&dir)?;
            if free_space(&dir).is_some_and(|free| free < size.max(0) as u64 + DISK_RESERVE) {
                return self.fail(rowid, Error::DiskFull).await;
            }
            match self
                .fetch_file(&client, track_rowid, &track_id, pin.quality)
                .await
            {
                Ok(()) => self.emit(Event::Changed(rowid)),
                Err(Error::Subsonic(lio_subsonic::Error::NotFound)) => {
                    // Gone from the server: drop it from this pin.
                    self.inner
                        .db
                        .run(move |c| {
                            c.execute(
                                "DELETE FROM download_track WHERE download_rowid = ?1 AND track_rowid = ?2",
                                params![rowid, track_rowid],
                            )?;
                            Ok(())
                        })
                        .await?;
                    self.emit(Event::TracksRemoved {
                        pin: rowid,
                        count: 1,
                    });
                }
                Err(Error::Subsonic(e)) if e.is_connectivity() => {
                    self.inner
                        .db
                        .run(move |c| set_status(c, rowid, &Status::Waiting))
                        .await?;
                    self.emit(Event::Changed(rowid));
                    return Ok(false);
                }
                Err(e) => return self.fail(rowid, e).await,
            }
        }
        let t = now();
        self.inner
            .db
            .run(move |c| {
                set_status(c, rowid, &Status::Complete)?;
                c.execute(
                    "UPDATE download SET last_synced_at = ?1 WHERE rowid = ?2",
                    params![t, rowid],
                )?;
                Ok(())
            })
            .await?;
        self.emit(Event::Changed(rowid));
        Ok(true)
    }

    async fn fail(&self, rowid: i64, error: Error) -> Result<bool> {
        let disk_full = matches!(error, Error::DiskFull);
        let message = if disk_full {
            DISK_FULL.to_owned()
        } else {
            error.to_string()
        };
        tracing::warn!(pin = rowid, "download failed: {message}");
        self.inner
            .db
            .run(move |c| set_status(c, rowid, &Status::Error(message)))
            .await?;
        self.emit(Event::Changed(rowid));
        if disk_full {
            self.emit(Event::DiskFull);
        }
        Ok(false)
    }

    /// Downloads one track into `downloads/<rowid>.<quality>`, resuming a `.part` file.
    async fn fetch_file(
        &self,
        client: &Client,
        track_rowid: i64,
        track_id: &str,
        quality: Quality,
    ) -> Result<()> {
        let dir = self.inner.paths.downloads();
        let final_path = dir.join(format!("{track_rowid}.{quality}"));
        let part = part_path(&final_path);
        let url = match quality {
            Quality::Original => client.download_url(track_id),
            q => {
                let (format, rate) = q.stream_params();
                client.stream_url(track_id, format, rate, None)
            }
        };
        // Transcodes aren't byte-identical between runs: only originals resume.
        let resume_from = if quality.is_transcoded() {
            let _ = std::fs::remove_file(&part);
            0
        } else {
            std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0)
        };
        let response = client.open_stream(url, resume_from).await?;
        let append = response.partial && response.start == resume_from;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&part)
            .map_err(Error::from_io)?;
        let mut body = response.response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| client.map_transport_error(e))?;
            file.write_all(&chunk).map_err(Error::from_io)?;
        }
        file.sync_all().map_err(Error::from_io)?;
        drop(file);
        std::fs::rename(&part, &final_path).map_err(Error::from_io)?;
        let size = std::fs::metadata(&final_path)?.len() as i64;
        let path = final_path.to_string_lossy().into_owned();
        self.inner
            .db
            .run(move |c| {
                c.execute(
                    "INSERT INTO download_file (track_rowid, quality, path, size_bytes, complete) VALUES (?1, ?2, ?3, ?4, 1)
                     ON CONFLICT (track_rowid, quality) DO UPDATE SET path = excluded.path,
                        size_bytes = excluded.size_bytes, complete = 1",
                    params![track_rowid, quality.as_str(), path, size],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }
}
