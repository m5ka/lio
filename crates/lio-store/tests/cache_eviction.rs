//! Cache eviction.

use std::collections::HashSet;

use lio_store::cache::{self, enforce_limit};
use lio_store::{Db, library};
use lio_subsonic::types::{Album, Song};
use rusqlite::params;

const MB: u64 = 1024 * 1024;

struct Fixture {
    _tmp: tempfile::TempDir,
    db: Db,
    dir: std::path::PathBuf,
    rowids: Vec<i64>,
}

/// Ten 10 MB cached tracks: track i last played `i` days ago, played `10 - i` times.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("audio");
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open_in_memory().unwrap();
    let album = Album {
        id: "al".into(),
        name: "A".into(),
        song: (0..10)
            .map(|i| Song {
                id: format!("s{i}"),
                title: format!("S{i}"),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let album_rowid = db
        .with(|c| library::upsert_album_with_tracks(c, &album))
        .unwrap();
    let rowids: Vec<i64> = db
        .with(|c| lio_store::queries::album_track_rows(c, album_rowid))
        .unwrap()
        .iter()
        .map(|t| t.rowid)
        .collect();
    let now = lio_store::db::now();
    for (i, rowid) in rowids.iter().enumerate() {
        let path = dir.join(format!("{rowid}.original"));
        std::fs::write(&path, vec![0u8; 16]).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO cache_entry (track_rowid, quality, path, size_bytes, complete, last_played_at, play_count, created_at)
                 VALUES (?1, 'original', ?2, ?3, 1, ?4, ?5, 0)",
                params![rowid, path.to_string_lossy(), (10 * MB) as i64, now - (i as i64) * 86_400, 10 - i as i64],
            )?;
            Ok(())
        })
        .unwrap();
    }
    Fixture {
        _tmp: tmp,
        db,
        dir,
        rowids,
    }
}

fn cached(f: &Fixture) -> Vec<i64> {
    f.db.with(|c| {
        let mut stmt = c.prepare("SELECT track_rowid FROM cache_entry ORDER BY track_rowid")?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(ids)
    })
    .unwrap()
}

#[test]
fn evicts_least_used_until_under_limit() {
    let f = fixture();
    let now = lio_store::db::now();
    let freed =
        f.db.with(|c| enforce_limit(c, &f.dir, 65 * MB, &HashSet::new(), &HashSet::new(), now))
            .unwrap();
    assert_eq!(freed, 40 * MB);
    let left = cached(&f);
    assert_eq!(
        left,
        f.rowids[..6].to_vec(),
        "the four oldest, least-played entries went first"
    );
    assert!(
        !f.dir.join(format!("{}.original", f.rowids[9])).exists(),
        "files are deleted too"
    );
    let usage = f.db.with(|c| cache::usage(c, &f.dir)).unwrap();
    assert!(usage.cache_bytes <= 65 * MB);
}

#[test]
fn protected_tracks_and_downloads_survive() {
    let f = fixture();
    // A download of the oldest track: not part of the cache, never evicted.
    let dl = f.dir.join("download");
    std::fs::write(&dl, b"x").unwrap();
    f.db.with(|c| {
        c.execute(
            "INSERT INTO download_file (track_rowid, quality, path, size_bytes, complete) VALUES (?1, 'original', ?2, ?3, 1)",
            params![f.rowids[9], dl.to_string_lossy(), (50 * MB) as i64],
        )?;
        Ok(())
    })
    .unwrap();
    let protected: HashSet<i64> = [f.rowids[9], f.rowids[8]].into();
    let now = lio_store::db::now();
    f.db.with(|c| enforce_limit(c, &f.dir, 20 * MB, &protected, &HashSet::new(), now))
        .unwrap();
    let left = cached(&f);
    assert!(
        left.contains(&f.rowids[9]) && left.contains(&f.rowids[8]),
        "current and next stay"
    );
    assert_eq!(left.len(), 2);
    let download_rows: i64 =
        f.db.with(|c| Ok(c.query_row("SELECT COUNT(*) FROM download_file", [], |r| r.get(0))?))
            .unwrap();
    assert_eq!(download_rows, 1);
    assert!(dl.exists(), "download files are never touched");
}

#[test]
fn partial_files_go_first_and_limit_holds_within_one_track() {
    let f = fixture();
    let part = f.dir.join("99.original.part");
    std::fs::write(&part, vec![0u8; (5 * MB) as usize]).unwrap();
    let active = f.dir.join("98.original.part");
    std::fs::write(&active, vec![0u8; MB as usize]).unwrap();
    let now = lio_store::db::now();
    let active_parts: HashSet<_> = [active.clone()].into();
    // 100 MB cached + 5 MB stray part + 1 MB active part = 106 MB; with a 101 MB limit,
    // removing the stray part is enough.
    let freed =
        f.db.with(|c| enforce_limit(c, &f.dir, 101 * MB, &HashSet::new(), &active_parts, now))
            .unwrap();
    assert_eq!(freed, 5 * MB);
    assert!(!part.exists());
    assert!(active.exists(), "a running fetch's file stays");
    assert_eq!(cached(&f).len(), 10);

    // Final total ≤ limit + one track.
    f.db.with(|c| enforce_limit(c, &f.dir, 33 * MB, &HashSet::new(), &active_parts, now))
        .unwrap();
    let usage = f.db.with(|c| cache::usage(c, &f.dir)).unwrap();
    assert!(usage.cache_bytes <= 33 * MB + 10 * MB, "{usage:?}");
}

#[test]
fn clear_keeps_protected() {
    let f = fixture();
    let protected: HashSet<i64> = [f.rowids[0]].into();
    f.db.with(|c| cache::clear(c, &f.dir, &protected, &HashSet::new()))
        .unwrap();
    assert_eq!(cached(&f), vec![f.rowids[0]]);
}
