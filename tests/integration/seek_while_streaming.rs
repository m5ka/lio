//! Against a real Navidrome behind a slow link: scrubbing through a track that is still
//! streaming must keep it playing.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lio_playback::{Player, TrackResolver, TrackSource};
use lio_store::{AccountPaths, Db, Quality, TrackHandle, TrackStore, library};
use lio_subsonic::{AlbumListType, Client, Credentials, TlsConfig};

use crate::common;

/// A TCP proxy to `upstream` that throttles responses to `bytes_per_sec`. Returns its
/// base URL.
fn throttled_proxy(upstream: &str, bytes_per_sec: usize) -> String {
    let upstream = upstream
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            let Ok(server) = TcpStream::connect(&upstream) else {
                continue;
            };
            let (mut c_read, mut s_write) =
                (client.try_clone().unwrap(), server.try_clone().unwrap());
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut c_read, &mut s_write);
                let _ = s_write.shutdown(Shutdown::Write);
            });
            let (mut s_read, mut c_write) = (server, client);
            std::thread::spawn(move || {
                let chunk = bytes_per_sec / 50;
                let mut buf = vec![0u8; chunk];
                loop {
                    match s_read.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if c_write.write_all(&buf[..n]).is_err() {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                }
                let _ = c_write.shutdown(Shutdown::Both);
                let _ = s_read.shutdown(Shutdown::Both);
            });
        }
    });
    url
}

struct HandleSource(TrackHandle);

impl TrackSource for HandleSource {
    fn size(&self) -> Option<u64> {
        self.0.size()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read_at(offset, buf)
    }
    fn cancel(&self) {
        self.0.cancel();
    }
    fn resume(&self) {
        self.0.resume();
    }
}

struct StoreResolver(TrackStore);

impl TrackResolver for StoreResolver {
    fn open(&self, track_id: &str, quality: &str) -> Result<Box<dyn TrackSource>, String> {
        let quality = Quality::parse(quality).unwrap_or_default();
        self.0
            .open_by_id(track_id, quality)
            .map(|h| Box::new(HandleSource(h)) as Box<dyn TrackSource>)
            .map_err(|e| e.to_string())
    }
}

/// Waits until playback is past `ms`, or gives up after `timeout`.
fn wait_past(player: &Player, ms: u64, timeout: Duration) -> Result<u64, String> {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < deadline {
        last = player.position_ms();
        if let Some(p) = last.filter(|p| *p > ms) {
            return Ok(p);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("stuck: position {last:?} never passed {ms} ms"))
}

/// Checks that playback resumes from close to `target` after a seek: it passes
/// `target + 1 s` in time, and the first position seen past that point isn't far beyond it.
fn landed_near(player: &Player, target: u64) -> Result<(), String> {
    let pos = wait_past(player, target + 1_000, Duration::from_secs(10))?;
    if pos > target + 2_500 {
        return Err(format!("seek to {target} ms landed at {pos} ms"));
    }
    Ok(())
}

/// Every state and buffering message so far, as the controller would see them.
fn drain_events(player: &Player) -> String {
    let bus = player.bus();
    let mut events = Vec::new();
    while let Some(msg) = bus.pop() {
        if let gst::MessageView::Buffering(b) = msg.view() {
            events.push(format!("buffering {}%", b.percent()));
        } else if let Some(event) = player.translate_message(&msg) {
            events.push(format!("{event:?}"));
        }
    }
    events.join(", ")
}

/// Plays `title` from `album_name` through a link of `bytes_per_sec`, drags the
/// slider across it and checks it keeps playing.
async fn scrub(album_name: &str, title: &str, bytes_per_sec: usize) -> Result<(), String> {
    let srv = common::server();
    let proxy = throttled_proxy(&srv.url, bytes_per_sec);
    let client = Client::new(
        &proxy,
        Credentials::token(&srv.user, &srv.password),
        TlsConfig::System,
    )
    .unwrap();
    let albums = client
        .get_album_list2(&AlbumListType::AlphabeticalByName, 500, 0)
        .await
        .unwrap();
    let id = albums
        .iter()
        .find(|a| a.name == album_name)
        .expect("fixture album")
        .id
        .clone();
    let album = client.get_album(&id).await.unwrap();
    let track = album
        .song
        .iter()
        .find(|s| s.title == title)
        .expect("fixture track")
        .clone();
    let duration_ms = track.duration.max(1) as u64 * 1000;

    lio_playback::init().unwrap();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        // A fresh cache, so every seek races the download.
        let xdg = common::temp_xdg();
        let paths = AccountPaths::with_roots(&proxy, "lio", &xdg.data_home(), &xdg.cache_home());
        paths.create_all().unwrap();
        let db = Db::open(&paths.db()).unwrap();
        db.with(|c| library::upsert_album_with_tracks(c, &album))
            .unwrap();
        let store = TrackStore::new(db, paths, runtime);
        store.set_client(Some(client));
        lio_playback::set_resolver(Arc::new(StoreResolver(store)));

        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", true)
            .build()
            .unwrap();
        let player = Player::new(Some(sink)).unwrap();
        let uri = lio_playback::track_uri(&track.id, Quality::Original.as_str());
        player.play_uri(&uri, None, false).unwrap();
        let result = (|| {
            wait_past(&player, 500, Duration::from_secs(10))?;
            // Drag the slider to 80 %: a seek per value change, ~60 per second.
            let end = duration_ms * 8 / 10;
            let mut target = 0;
            for step in 1..=30 {
                target = 1_000 + (end - 1_000) * step / 30;
                player.seek_ms(target);
                std::thread::sleep(Duration::from_millis(16));
            }
            landed_near(&player, target)?;
            // Then a jump back near the start.
            player.seek_ms(1_000);
            landed_near(&player, 1_000)
        })();
        let events = drain_events(&player);
        player.stop();
        result.map_err(|e| format!("{e}; events: {events}"))
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs tests/test-server.sh up"]
async fn scrubbing_a_streaming_track_keeps_playing() {
    // Links faster than playback but slower than the file, so seeks land on bytes that
    // haven't arrived yet.
    let cases = [
        ("Gapless Sweep", "Sweep Part 4", 400 * 1024),
        ("Format Mix", "FLAC", 72 * 1024),
        ("Format Mix", "MP3", 160 * 1024),
        ("Format Mix", "Opus", 80 * 1024),
        ("Long Tracks", "Long MP3", 100 * 1024),
        ("Long Tracks", "Long FLAC", 300 * 1024),
    ];
    let mut failures = Vec::new();
    let only = std::env::var("LIO_SCRUB_ONLY").ok();
    for (album, title, rate) in cases {
        if only.as_deref().is_some_and(|o| o != title) {
            continue;
        }
        for round in 0..3 {
            match scrub(album, title, rate).await {
                Ok(()) => eprintln!("{title} round {round}: ok"),
                Err(e) => {
                    eprintln!("{title} round {round}: {e}");
                    failures.push(format!("{title}: {e}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
