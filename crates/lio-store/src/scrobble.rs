//! Scrobbling.
//!
//! [`PlayTracker`] accumulates real playing time per play instance and reports a play
//! exactly once, when it reaches `min(duration / 2, 240 s)`. Plays wait in the
//! `scrobble` table until [`submit_pending`] delivers them, in order, so plays made
//! while offline arrive later. Submitted rows are purged after 7 days.

use lio_subsonic::Client;
use rusqlite::{Connection, params};

use crate::db::{Db, now};
use crate::error::Result;

/// A play counts after this much listening at most.
pub const MAX_THRESHOLD_MS: u64 = 240_000;
/// Position jumps larger than this are seeks, not listening time.
const MAX_TICK_MS: u64 = 2_000;
const PURGE_AFTER_S: i64 = 7 * 24 * 3600;

/// Listening threshold for a track (`min(duration / 2, 240 s)`).
pub fn threshold_ms(duration_s: i64) -> u64 {
    ((duration_s.max(0) as u64) * 1000 / 2).min(MAX_THRESHOLD_MS)
}

#[derive(Debug, Clone)]
struct Play {
    id: uuid::Uuid,
    track_id: String,
    started_at: i64,
    threshold_ms: u64,
    listened_ms: u64,
    last_position_ms: Option<u64>,
    recorded: bool,
}

/// A play that crossed its threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedPlay {
    pub play_id: uuid::Uuid,
    pub track_id: String,
    /// Unix seconds when the play started.
    pub played_at: i64,
}

#[derive(Debug, Default)]
pub struct PlayTracker {
    current: Option<Play>,
}

impl PlayTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new play instance began (also for repeat-one replays of the same track).
    pub fn start(&mut self, track_id: &str, duration_s: i64, started_at: i64) {
        self.current = Some(Play {
            id: uuid::Uuid::new_v4(),
            track_id: track_id.to_owned(),
            started_at,
            threshold_ms: threshold_ms(duration_s),
            listened_ms: 0,
            last_position_ms: None,
            recorded: false,
        });
    }

    pub fn stop(&mut self) {
        self.current = None;
    }

    /// Feeds the playback position. Only forward progress while playing counts;
    /// seeks and pauses don't. Returns the play once, when it crosses the threshold.
    pub fn tick(&mut self, position_ms: u64, playing: bool) -> Option<CompletedPlay> {
        let play = self.current.as_mut()?;
        let last = play.last_position_ms.replace(position_ms);
        if playing
            && let Some(last) = last
            && position_ms > last
            && position_ms - last <= MAX_TICK_MS
        {
            play.listened_ms += position_ms - last;
        }
        if !play.recorded && play.threshold_ms > 0 && play.listened_ms >= play.threshold_ms {
            play.recorded = true;
            return Some(CompletedPlay {
                play_id: play.id,
                track_id: play.track_id.clone(),
                played_at: play.started_at,
            });
        }
        None
    }

    /// The pause button: the next tick must not count the time spent paused.
    pub fn pause(&mut self) {
        if let Some(play) = self.current.as_mut() {
            play.last_position_ms = None;
        }
    }
}

/// Queues a completed play for submission.
pub fn enqueue(conn: &Connection, play: &CompletedPlay) -> Result<()> {
    conn.execute(
        "INSERT INTO scrobble (track_id, played_at, submitted, attempts) VALUES (?1, ?2, 0, 0)",
        params![play.track_id, play.played_at],
    )?;
    Ok(())
}

/// Unsubmitted plays, oldest first: `(rowid, track_id, played_at)`.
pub fn pending(conn: &Connection, limit: usize) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT rowid, track_id, played_at FROM scrobble WHERE NOT submitted ORDER BY played_at, rowid LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

pub fn mark_submitted(conn: &Connection, rowid: i64, at: i64) -> Result<()> {
    conn.execute(
        "UPDATE scrobble SET submitted = 1, submitted_at = ?1 WHERE rowid = ?2",
        params![at, rowid],
    )?;
    Ok(())
}

pub fn mark_failed(conn: &Connection, rowid: i64) -> Result<()> {
    conn.execute(
        "UPDATE scrobble SET attempts = attempts + 1 WHERE rowid = ?1",
        [rowid],
    )?;
    Ok(())
}

/// Deletes submitted plays older than 7 days.
pub fn purge(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM scrobble WHERE submitted AND submitted_at < ?1",
        [now - PURGE_AFTER_S],
    )?)
}

/// Sends pending plays in order. Stops at the first failure (the caller backs off) and
/// returns how many were delivered.
pub async fn submit_pending(client: &Client, db: &Db) -> Result<usize> {
    let pending = db.run(|c| pending(c, 100)).await?;
    let mut sent = 0;
    for (rowid, track_id, played_at) in pending {
        match client
            .scrobble(&track_id, true, Some(played_at * 1000))
            .await
        {
            Ok(()) => {
                db.run(move |c| mark_submitted(c, rowid, now())).await?;
                sent += 1;
            }
            Err(lio_subsonic::Error::NotFound) => {
                // The track is gone from the server: drop the play.
                db.run(move |c| {
                    Ok(c.execute("DELETE FROM scrobble WHERE rowid = ?1", [rowid])
                        .map(drop)?)
                })
                .await?;
            }
            Err(e) => {
                db.run(move |c| mark_failed(c, rowid)).await?;
                return Err(e.into());
            }
        }
    }
    db.run(|c| purge(c, now()).map(drop)).await?;
    Ok(sent)
}
