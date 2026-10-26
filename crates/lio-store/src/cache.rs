//! Audio cache accounting and eviction.
//!
//! Usage counts complete cache entries plus stray `.part` files. When it exceeds the
//! limit, partial files go first, then complete entries by
//! `score = (now − last_played_at) / (1 + ln(1 + play_count))`, highest first. The
//! current and next tracks (protected) and every download are never touched.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::error::Result;

/// Bytes used by the audio cache and by downloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub cache_bytes: u64,
    pub download_bytes: u64,
}

/// Eviction score: older and less played is evicted first.
pub fn score(now: i64, last_played_at: i64, play_count: i64) -> f64 {
    let age = (now - last_played_at).max(0) as f64;
    age / (1.0 + (1.0 + play_count.max(0) as f64).ln())
}

/// `.part` files in `dir` that no running fetch owns, with their sizes.
fn stray_parts(dir: &Path, active: &HashSet<PathBuf>) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "part") && !active.contains(p))
        .filter_map(|p| std::fs::metadata(&p).ok().map(|m| (p, m.len())))
        .collect()
}

pub fn usage(conn: &Connection, cache_dir: &Path) -> Result<Usage> {
    let cache: i64 = conn.query_row(
        "SELECT COALESCE(SUM(size_bytes), 0) FROM cache_entry WHERE complete",
        [],
        |r| r.get(0),
    )?;
    let downloads: i64 = conn.query_row(
        "SELECT COALESCE(SUM(size_bytes), 0) FROM download_file WHERE complete",
        [],
        |r| r.get(0),
    )?;
    let parts: u64 = stray_parts(cache_dir, &HashSet::new())
        .iter()
        .map(|(_, n)| n)
        .sum();
    Ok(Usage {
        cache_bytes: cache.max(0) as u64 + parts,
        download_bytes: downloads.max(0) as u64,
    })
}

/// Evicts until the cache fits `limit_bytes`. `protected` are track rowids that must
/// stay (current, next); `active_parts` are `.part` files of running fetches.
/// Returns the number of bytes freed.
pub fn enforce_limit(
    conn: &Connection,
    cache_dir: &Path,
    limit_bytes: u64,
    protected: &HashSet<i64>,
    active_parts: &HashSet<PathBuf>,
    now: i64,
) -> Result<u64> {
    let mut total = usage(conn, cache_dir)?.cache_bytes;
    let mut freed = 0;
    if total <= limit_bytes {
        return Ok(0);
    }
    // 1. Partial files nobody is writing.
    for (path, size) in stray_parts(cache_dir, active_parts) {
        if total <= limit_bytes {
            return Ok(freed);
        }
        let _ = std::fs::remove_file(&path);
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(".ranges");
        let _ = std::fs::remove_file(PathBuf::from(sidecar));
        total = total.saturating_sub(size);
        freed += size;
    }
    // 2. Complete entries by score, highest first.
    let mut entries: Vec<(i64, String, String, i64, i64, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT track_rowid, quality, path, size_bytes, last_played_at, play_count FROM cache_entry WHERE complete",
        )?;
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?
    };
    entries.sort_by(|a, b| score(now, b.4, b.5).total_cmp(&score(now, a.4, a.5)));
    for (rowid, quality, path, size, _, _) in entries {
        if total <= limit_bytes {
            break;
        }
        if protected.contains(&rowid) {
            continue;
        }
        conn.execute(
            "DELETE FROM cache_entry WHERE track_rowid = ?1 AND quality = ?2",
            params![rowid, quality],
        )?;
        let _ = std::fs::remove_file(&path);
        let size = size.max(0) as u64;
        total = total.saturating_sub(size);
        freed += size;
    }
    if freed > 0 {
        tracing::info!(
            freed,
            total,
            limit = limit_bytes,
            "evicted from audio cache"
        );
    }
    Ok(freed)
}

/// Removes every cache entry except protected ones (Preferences → Clear Cache).
pub fn clear(
    conn: &Connection,
    cache_dir: &Path,
    protected: &HashSet<i64>,
    active_parts: &HashSet<PathBuf>,
) -> Result<u64> {
    enforce_limit(
        conn,
        cache_dir,
        0,
        protected,
        active_parts,
        crate::db::now(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_orders_old_and_rarely_played_first() {
        let now = 1_000_000;
        let old_once = score(now, now - 100_000, 1);
        let old_often = score(now, now - 100_000, 50);
        let recent_once = score(now, now - 100, 1);
        assert!(old_once > old_often, "frequently played survives longer");
        assert!(old_once > recent_once, "recently played survives longer");
        assert_eq!(
            score(now, now + 10, 0),
            0.0,
            "clock skew doesn't go negative"
        );
    }
}
