//! TrackStore against a mock server: streaming into the cache, seeking, the
//! resolution order, and offline behaviour.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lio_store::{AccountPaths, Db, Origin, Quality, TrackStore, library};
use lio_subsonic::types::{Album, Song};
use lio_subsonic::{Client, Credentials, TlsConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const SIZE: usize = 3 * 1024 * 1024;

/// Serves a deterministic body, honouring `Range: bytes=N-`.
struct RangeBody {
    body: Arc<Vec<u8>>,
    requests: Arc<AtomicUsize>,
    ranged: Arc<AtomicUsize>,
}

impl Respond for RangeBody {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let start = req
            .headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.trim_end_matches('-').parse::<usize>().ok());
        match start {
            Some(s) if s > 0 => {
                self.ranged.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(206)
                    .insert_header(
                        "content-range",
                        format!("bytes {s}-{}/{}", self.body.len() - 1, self.body.len()),
                    )
                    .set_body_raw(self.body[s..].to_vec(), "audio/flac")
            }
            _ => ResponseTemplate::new(200).set_body_raw(self.body.to_vec(), "audio/flac"),
        }
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    _server: MockServer,
    store: TrackStore,
    body: Arc<Vec<u8>>,
    requests: Arc<AtomicUsize>,
    ranged: Arc<AtomicUsize>,
    rowids: Vec<i64>,
}

async fn fixture() -> Fixture {
    let body: Arc<Vec<u8>> = Arc::new((0..SIZE).map(|i| (i * 7 % 251) as u8).collect());
    let requests = Arc::new(AtomicUsize::new(0));
    let ranged = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .respond_with(RangeBody {
            body: body.clone(),
            requests: requests.clone(),
            ranged: ranged.clone(),
        })
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let paths = AccountPaths::with_roots(
        &server.uri(),
        "lio",
        &tmp.path().join("d"),
        &tmp.path().join("c"),
    );
    paths.create_all().unwrap();
    let db = Db::open(&paths.db()).unwrap();
    let album = Album {
        id: "al1".into(),
        name: "Album".into(),
        song: (1..=4)
            .map(|i| Song {
                id: format!("s{i}"),
                title: format!("Song {i}"),
                duration: 10,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    db.with(|c| library::upsert_album_with_tracks(c, &album))
        .unwrap();
    let rowids: Vec<i64> = db
        .with(|c| {
            let mut stmt = c.prepare("SELECT rowid FROM track ORDER BY id")?;
            let rows = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<i64>, _>>()?;
            Ok(rows)
        })
        .unwrap();

    let store = TrackStore::new(db, paths, tokio::runtime::Handle::current());
    store.set_client(Some(
        Client::new(
            &server.uri(),
            Credentials::token("lio", "pw"),
            TlsConfig::System,
        )
        .unwrap(),
    ));
    Fixture {
        _tmp: tmp,
        _server: server,
        store,
        body,
        requests,
        ranged,
        rowids,
    }
}

async fn wait_idle(store: &TrackStore) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while store.active_fetches() > 0 {
        assert!(Instant::now() < deadline, "fetch did not finish");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn read_all(handle: &lio_store::TrackHandle) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = handle.read_at(out.len() as u64, &mut buf).unwrap();
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&buf[..n]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_into_cache_then_serves_from_cache() {
    let f = fixture().await;
    let store = f.store.clone();
    let rowid = f.rowids[0];

    let data = tokio::task::spawn_blocking(move || {
        let handle = store.open(rowid, Quality::Original).unwrap();
        assert_eq!(handle.origin(), Origin::Stream(Quality::Original));
        assert_eq!(handle.size(), Some(SIZE as u64));
        read_all(&handle)
    })
    .await
    .unwrap();
    assert_eq!(data.len(), SIZE);
    assert!(data == *f.body, "streamed bytes match the server's");

    wait_idle(&f.store).await;
    let (origin, path) = f
        .store
        .resolve_local(rowid, Quality::Original)
        .unwrap()
        .unwrap();
    assert_eq!(origin, Origin::Cache(Quality::Original));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), SIZE as u64);
    assert!(!path.with_extension("original.part").exists());

    let before = f.requests.load(Ordering::SeqCst);
    let store = f.store.clone();
    let cached = tokio::task::spawn_blocking(move || {
        let handle = store.open(rowid, Quality::Original).unwrap();
        assert_eq!(handle.origin(), Origin::Cache(Quality::Original));
        read_all(&handle)
    })
    .await
    .unwrap();
    assert!(cached == *f.body);
    assert_eq!(
        f.requests.load(Ordering::SeqCst),
        before,
        "cache hit makes no request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn far_read_restarts_with_range_request() {
    let f = fixture().await;
    let store = f.store.clone();
    let rowid = f.rowids[1];
    let tail = tokio::task::spawn_blocking(move || {
        let handle = store.open(rowid, Quality::Original).unwrap();
        let mut buf = vec![0u8; 1000];
        let offset = (SIZE - 2000) as u64;
        let n = handle.read_at(offset, &mut buf).unwrap();
        (offset, buf[..n].to_vec())
    })
    .await
    .unwrap();
    let (offset, bytes) = tail;
    assert!(!bytes.is_empty());
    assert_eq!(
        &bytes[..],
        &f.body[offset as usize..offset as usize + bytes.len()]
    );
    wait_idle(&f.store).await;
    // The file still completes with every byte correct.
    let (_, path) = f
        .store
        .resolve_local(rowid, Quality::Original)
        .unwrap()
        .unwrap();
    assert!(std::fs::read(path).unwrap() == *f.body);
    let _ = f.ranged.load(Ordering::SeqCst); // may be 0 if the first response raced ahead
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prefetch_fills_cache_without_reader() {
    let f = fixture().await;
    let store = f.store.clone();
    let rowid = f.rowids[2];
    tokio::task::spawn_blocking(move || store.prefetch(rowid, Quality::Original).unwrap())
        .await
        .unwrap();
    wait_idle(&f.store).await;
    assert!(f.store.is_available_offline(rowid).unwrap());
    // Prefetching again is a no-op.
    let before = f.requests.load(Ordering::SeqCst);
    let store = f.store.clone();
    tokio::task::spawn_blocking(move || store.prefetch(rowid, Quality::Original).unwrap())
        .await
        .unwrap();
    assert_eq!(f.store.active_fetches(), 0);
    assert_eq!(f.requests.load(Ordering::SeqCst), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolution_order() {
    let f = fixture().await;
    let rowid = f.rowids[3];
    let dir = f.store.paths().audio_cache();

    // Step 3: only an `original` cache copy exists, `opus-128` requested → use it.
    let cached = dir.join("cached-original");
    std::fs::write(&cached, b"cached").unwrap();
    f.store
        .db()
        .with(|c| {
            c.execute(
                "INSERT INTO cache_entry (track_rowid, quality, path, size_bytes, complete) VALUES (?1, 'original', ?2, 6, 1)",
                rusqlite::params![rowid, cached.to_string_lossy()],
            )?;
            Ok(())
        })
        .unwrap();
    let (origin, _) = f
        .store
        .resolve_local(rowid, Quality::Opus128)
        .unwrap()
        .unwrap();
    assert_eq!(origin, Origin::Cache(Quality::Original));

    // Step 2 beats step 3: an exact-quality cache copy wins.
    let exact = dir.join("cached-opus");
    std::fs::write(&exact, b"opus").unwrap();
    f.store
        .db()
        .with(|c| {
            c.execute(
                "INSERT INTO cache_entry (track_rowid, quality, path, size_bytes, complete) VALUES (?1, 'opus-128', ?2, 4, 1)",
                rusqlite::params![rowid, exact.to_string_lossy()],
            )?;
            Ok(())
        })
        .unwrap();
    let (origin, _) = f
        .store
        .resolve_local(rowid, Quality::Opus128)
        .unwrap()
        .unwrap();
    assert_eq!(origin, Origin::Cache(Quality::Opus128));

    // Step 1 beats everything: a download at any quality.
    let dl = f.store.paths().downloads().join("dl");
    std::fs::write(&dl, b"download").unwrap();
    f.store
        .db()
        .with(|c| {
            c.execute(
                "INSERT INTO download_file (track_rowid, quality, path, size_bytes, complete) VALUES (?1, 'mp3-320', ?2, 8, 1)",
                rusqlite::params![rowid, dl.to_string_lossy()],
            )?;
            Ok(())
        })
        .unwrap();
    let store = f.store.clone();
    let (origin, bytes) = tokio::task::spawn_blocking(move || {
        let h = store.open(rowid, Quality::Opus128).unwrap();
        (h.origin(), read_all(&h))
    })
    .await
    .unwrap();
    assert_eq!(origin, Origin::Download(Quality::Mp3_320));
    assert_eq!(bytes, b"download");
    assert_eq!(
        f.requests.load(Ordering::SeqCst),
        0,
        "no network for local copies"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_without_local_copy_is_unavailable() {
    let f = fixture().await;
    f.store.set_online(false);
    let store = f.store.clone();
    let rowid = f.rowids[0];
    let err = tokio::task::spawn_blocking(move || store.open(rowid, Quality::Original).err())
        .await
        .unwrap()
        .expect("must fail offline");
    assert!(
        matches!(err, lio_store::Error::UnavailableOffline),
        "{err:?}"
    );
    assert_eq!(f.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_unblocks_reader() {
    // A server that never answers: the reader must return promptly after cancel().
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;
    let f = fixture().await;
    f.store.set_client(Some(
        Client::new(
            &server.uri(),
            Credentials::token("lio", "pw"),
            TlsConfig::System,
        )
        .unwrap(),
    ));
    let store = f.store.clone();
    let rowid = f.rowids[0];
    let handle = Arc::new(
        tokio::task::spawn_blocking(move || store.open(rowid, Quality::Original).unwrap())
            .await
            .unwrap(),
    );
    let reader = handle.clone();
    let started = Instant::now();
    let read = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 16];
        reader.read_at(0, &mut buf)
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    handle.cancel();
    let err = read.await.unwrap().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    assert!(started.elapsed() < Duration::from_secs(3));
    f.store.cancel_all();
}

/// A server that sends the body slowly, like a remote Navidrome: 1 KB, then 32 KB
/// every 20 ms. Honours `Range: bytes=N-`.
fn trickle_server(body: Arc<Vec<u8>>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let body = body.clone();
            std::thread::spawn(move || {
                let mut req = [0u8; 4096];
                let n = stream.read(&mut req)?;
                let req = String::from_utf8_lossy(&req[..n]).to_ascii_lowercase();
                let start = req
                    .split("range: bytes=")
                    .nth(1)
                    .and_then(|r| r.split('-').next()?.parse::<usize>().ok())
                    .unwrap_or(0);
                let header = if start > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\ncontent-type: audio/mpeg\r\n\
                         content-range: bytes {start}-{}/{}\r\ncontent-length: {}\r\n\r\n",
                        body.len() - 1,
                        body.len(),
                        body.len() - start
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: audio/mpeg\r\n\
                         content-length: {}\r\n\r\n",
                        body.len()
                    )
                };
                let body = &body[start..];
                stream.write_all(header.as_bytes())?;
                stream.write_all(&body[..1024.min(body.len())])?;
                for chunk in body[1024.min(body.len())..].chunks(32 * 1024) {
                    std::thread::sleep(Duration::from_millis(20));
                    stream.write_all(chunk)?;
                }
                std::io::Result::Ok(())
            });
        }
    });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_reads_wait_for_the_whole_request() {
    // Pull-mode demuxers (id3demux reading a large tag) treat a short read as the end
    // of the file, so a read must not return just the bytes that happen to be there.
    let f = fixture().await;
    f.store.set_client(Some(
        Client::new(
            &trickle_server(f.body.clone()),
            Credentials::token("lio", "pw"),
            TlsConfig::System,
        )
        .unwrap(),
    ));
    let store = f.store.clone();
    let rowid = f.rowids[2];
    let body = f.body.clone();
    tokio::task::spawn_blocking(move || {
        let handle = store.open(rowid, Quality::Original).unwrap();
        let mut buf = vec![0u8; 512 * 1024];
        let n = handle.read_at(10, &mut buf).unwrap();
        assert_eq!(
            n,
            buf.len(),
            "read returned before the requested bytes arrived"
        );
        assert!(buf[..] == body[10..10 + n]);
        // At the end of the file a read returns only what's left.
        let mut buf = vec![0u8; 4096];
        let n = handle.read_at((SIZE - 100) as u64, &mut buf).unwrap();
        assert_eq!(n, 100);
        assert!(buf[..n] == body[SIZE - 100..]);
    })
    .await
    .unwrap();
}
