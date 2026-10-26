//! Synthetic large-library benchmark.
//!
//! ```text
//! cargo run --release -p lio-bench -- --tracks 100000
//! cargo run --release -p lio-bench -- --seed-server <dir> --tracks 20000
//! ```
//!
//! The first form builds a synthetic library database (N tracks, N/10 albums,
//! N/50 artists) through Lio's real write path, then times the queries the UI runs:
//! album ID lists in every sort order, one 200-row hydration batch (what a list view
//! loads per scroll step) and FTS searches; it reports process RSS at the end.
//!
//! `--seed-server <dir>` instead writes a matching music folder for the test Navidrome
//! (tiny FLAC files tagged with `metaflac`; slow for very large N).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use lio_store::queries::{self, AlbumFilter, AlbumSort};
use lio_store::{Db, library, search};
use lio_subsonic::SearchLimits;
use lio_subsonic::types::{Album, ArtistRef, Song};

/// Performance budgets, used for PASS/FAIL marks.
const SEARCH_BUDGET: Duration = Duration::from_millis(300);
const HYDRATE_BUDGET: Duration = Duration::from_millis(8);
const RSS_BUDGET_MB: u64 = 300;

const WORDS: &[&str] = &[
    "night", "river", "golden", "echo", "silent", "fire", "glass", "summer", "electric", "dream",
    "ocean", "shadow", "velvet", "neon", "winter", "paper", "storm", "crystal", "wild", "midnight",
    "orbit", "ember", "harbor", "signal", "lantern", "meadow",
];

struct Args {
    tracks: usize,
    seed_server: Option<PathBuf>,
    db: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut args = Args {
        tracks: 100_000,
        seed_server: None,
        db: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--tracks" => args.tracks = it.next().and_then(|v| v.parse().ok()).expect("--tracks N"),
            "--seed-server" => {
                args.seed_server = Some(it.next().expect("--seed-server DIR").into())
            }
            "--db" => args.db = Some(it.next().expect("--db PATH").into()),
            "-h" | "--help" => {
                println!("usage: lio-bench [--tracks N] [--db PATH] [--seed-server DIR]");
                std::process::exit(0);
            }
            other => panic!("unknown argument {other}"),
        }
    }
    args
}

/// Deterministic pseudo-random words so runs are comparable.
fn name(seed: usize, words: usize) -> String {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
    (0..words)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let w = WORDS[x % WORDS.len()];
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn synthetic_albums(tracks: usize) -> Vec<Album> {
    let albums = (tracks / 10).max(1);
    let artists = (tracks / 50).max(1);
    (0..albums)
        .map(|a| {
            let artist = a % artists;
            let artist_ref = ArtistRef {
                id: format!("ar{artist}"),
                name: name(artist + 7, 2),
            };
            Album {
                id: format!("al{a}"),
                name: name(a + 1000, 3),
                artist: Some(artist_ref.name.clone()),
                artist_id: Some(artist_ref.id.clone()),
                artists: vec![artist_ref.clone()],
                year: Some(1960 + (a % 65) as i64),
                genre: Some(WORDS[a % 8].to_owned()),
                song_count: 10,
                duration: 2400,
                play_count: Some((a % 37) as i64),
                created: Some(format!("2026-01-{:02}T00:00:00Z", 1 + a % 28)),
                song: (0..10)
                    .map(|t| Song {
                        id: format!("s{a}_{t}"),
                        title: name(a * 10 + t + 50_000, 2),
                        album: Some(String::new()),
                        artist: Some(artist_ref.name.clone()),
                        artists: vec![artist_ref.clone()],
                        track: Some(t as i64 + 1),
                        duration: 240,
                        size: 8_000_000,
                        suffix: Some("flac".into()),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }
        })
        .collect()
}

fn rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .map(str::to_owned)
        })
        .and_then(|l| {
            l.split_whitespace()
                .nth(1)
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .map(|kb| kb / 1024)
        .unwrap_or(0)
}

fn mark(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = f();
    (out, start.elapsed())
}

fn bench(tracks: usize, db_path: Option<&Path>) {
    let tmp = tempfile::tempdir().unwrap();
    let path = db_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| tmp.path().join("library.db"));
    let _ = std::fs::remove_file(&path);
    let db = Db::open(&path).unwrap();

    println!(
        "Building synthetic library: {tracks} tracks, {} albums, {} artists…",
        tracks / 10,
        tracks / 50
    );
    let albums = synthetic_albums(tracks);
    let (_, build) = time(|| {
        for chunk in albums.chunks(500) {
            db.with(|c| {
                let tx = c.transaction()?;
                for album in chunk {
                    library::upsert_album(&tx, album)?;
                }
                tx.commit()?;
                Ok(())
            })
            .unwrap();
        }
        for album in &albums {
            db.with(|c| library::upsert_album_with_tracks(c, album).map(drop))
                .unwrap();
        }
    });
    drop(albums);
    println!(
        "  built in {:.1} s ({:.0} MB on disk)\n",
        build.as_secs_f64(),
        std::fs::metadata(&path)
            .map(|m| m.len() / 1_048_576)
            .unwrap_or(0)
    );

    println!("Album ID lists (what a grid loads when the sort changes):");
    let mut all_ids = Vec::new();
    for sort in [
        AlbumSort::RecentlyAdded,
        AlbumSort::Name,
        AlbumSort::Artist,
        AlbumSort::Year,
        AlbumSort::MostPlayed,
        AlbumSort::Random,
    ] {
        let (ids, t) = time(|| {
            db.read(|c| queries::album_ids(c, sort, &AlbumFilter::All, None))
                .unwrap()
        });
        println!(
            "  {sort:<14?} {:>7} ids  {:>7.2} ms",
            ids.len(),
            t.as_secs_f64() * 1000.0
        );
        all_ids = ids;
    }

    println!("\nHydrating one 200-row batch (per scroll step):");
    let mid = all_ids.len() / 2;
    let batch = &all_ids[mid..(mid + 200).min(all_ids.len())];
    let (_, t) = time(|| db.read(|c| queries::album_rows(c, batch)).unwrap());
    println!(
        "  albums  {:>7.2} ms  {}",
        t.as_secs_f64() * 1000.0,
        mark(t <= HYDRATE_BUDGET)
    );
    let track_ids: Vec<i64> = db
        .read(|c| {
            let mut stmt = c.prepare("SELECT rowid FROM track ORDER BY random() LIMIT 200")?;
            let ids = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            Ok(ids)
        })
        .unwrap();
    let (_, t) = time(|| db.read(|c| queries::track_rows(c, &track_ids)).unwrap());
    println!(
        "  tracks  {:>7.2} ms  {}",
        t.as_secs_f64() * 1000.0,
        mark(t <= HYDRATE_BUDGET)
    );

    println!("\nSearch (FTS, offline):");
    for q in ["night", "gold ech", "mid", "zzz-no-match"] {
        let (r, t) = time(|| {
            db.read(|c| search::search_local(c, q, SearchLimits::default()))
                .unwrap()
        });
        println!(
            "  {q:<14} {:>3} artists {:>3} albums {:>3} tracks  {:>7.2} ms  {}",
            r.artists.len(),
            r.albums.len(),
            r.tracks.len(),
            t.as_secs_f64() * 1000.0,
            mark(t <= SEARCH_BUDGET)
        );
    }

    let rss = rss_mb();
    println!(
        "\nProcess RSS: {rss} MB  {} (budget {RSS_BUDGET_MB} MB while browsing)",
        mark(rss <= RSS_BUDGET_MB)
    );
    println!(
        "\nScroll smoothness is measured in the app: LIO_FRAME_STATS=1 LIO_AUTOSCROLL=albums lio"
    );
}

fn seed_server(dir: &Path, tracks: usize) {
    std::fs::create_dir_all(dir).unwrap();
    let template = dir.join(".template.flac");
    if !template.exists() {
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=8000:cl=mono",
                "-t",
                "1",
                "-c:a",
                "flac",
            ])
            .arg(&template)
            .status()
            .expect("ffmpeg is required for --seed-server");
        assert!(status.success());
    }
    let albums = synthetic_albums(tracks);
    let total = albums.len() * 10;
    let mut done = 0;
    for album in &albums {
        let artist = album.artist.clone().unwrap_or_default();
        let album_dir = dir.join(&artist).join(&album.name);
        std::fs::create_dir_all(&album_dir).unwrap();
        for song in &album.song {
            let file = album_dir.join(format!(
                "{:02} - {}.flac",
                song.track.unwrap_or(0),
                song.title
            ));
            std::fs::copy(&template, &file).unwrap();
            let status = Command::new("metaflac")
                .arg("--remove-all-tags")
                .arg(format!("--set-tag=ARTIST={artist}"))
                .arg(format!("--set-tag=ALBUMARTIST={artist}"))
                .arg(format!("--set-tag=ALBUM={}", album.name))
                .arg(format!("--set-tag=TITLE={}", song.title))
                .arg(format!("--set-tag=TRACKNUMBER={}", song.track.unwrap_or(0)))
                .arg(format!("--set-tag=DATE={}", album.year.unwrap_or(2000)))
                .arg(format!(
                    "--set-tag=GENRE={}",
                    album.genre.clone().unwrap_or_default()
                ))
                .arg(&file)
                .status()
                .expect("metaflac is required for --seed-server");
            assert!(status.success());
            done += 1;
        }
        if done % 1000 == 0 {
            eprintln!("  {done}/{total} files");
        }
    }
    let _ = std::fs::remove_file(&template);
    println!(
        "Wrote {total} tracks to {}. Point the test server at it with LIO_TEST_MUSIC={}",
        dir.display(),
        dir.display()
    );
}

fn main() {
    let args = parse_args();
    match &args.seed_server {
        Some(dir) => seed_server(dir, args.tracks),
        None => bench(args.tracks, args.db.as_deref()),
    }
}
