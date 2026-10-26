//! Against a real Navidrome: sign-in errors and streaming into the cache.

use std::time::{Duration, Instant};

use lio_store::{AccountPaths, Db, Origin, Quality, TrackStore, library, queries};
use lio_subsonic::{AlbumListType, Client, Credentials, Error, TlsConfig};

use crate::common;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs tests/test-server.sh up"]
async fn sign_in_and_stream_into_cache() {
    let srv = common::server();

    // Wrong password → BadCredentials; right password → ping ok.
    let bad = Client::new(
        &srv.url,
        Credentials::token(&srv.user, "wrong"),
        TlsConfig::System,
    )
    .unwrap();
    assert!(matches!(bad.ping().await, Err(Error::BadCredentials)));
    let client = Client::new(
        &srv.url,
        Credentials::token(&srv.user, &srv.password),
        TlsConfig::System,
    )
    .unwrap();
    let info = client.ping().await.expect("ping");
    assert_eq!(info.server_type, "navidrome");

    // Mirror the gapless album and stream its first track through the TrackStore.
    let albums = client
        .get_album_list2(&AlbumListType::AlphabeticalByName, 500, 0)
        .await
        .unwrap();
    let sweep = albums
        .iter()
        .find(|a| a.name == "Gapless Sweep")
        .expect("fixture album");
    let album = client.get_album(&sweep.id).await.unwrap();

    let xdg = common::temp_xdg();
    let paths = AccountPaths::with_roots(&srv.url, &srv.user, &xdg.data_home(), &xdg.cache_home());
    paths.create_all().unwrap();
    let db = Db::open(&paths.db()).unwrap();
    let album_rowid = db
        .with(|c| library::upsert_album_with_tracks(c, &album))
        .unwrap();
    let tracks = db
        .with(|c| queries::album_track_rows(c, album_rowid))
        .unwrap();
    assert_eq!(tracks.len(), 5);
    assert_eq!(tracks[0].track_no, Some(1));

    let store = TrackStore::new(db, paths.clone(), tokio::runtime::Handle::current());
    store.set_client(Some(client.clone()));
    let first = tracks[0].clone();
    let expected_size = first.size_bytes as u64;

    let reader = store.clone();
    let rowid = first.rowid;
    let (origin, bytes) = tokio::task::spawn_blocking(move || {
        let handle = reader.open(rowid, Quality::Original).unwrap();
        let origin = handle.origin();
        let mut total = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = handle.read_at(total, &mut buf).unwrap();
            if n == 0 {
                break;
            }
            total += n as u64;
        }
        (origin, total)
    })
    .await
    .unwrap();
    assert_eq!(origin, Origin::Stream(Quality::Original));
    assert_eq!(bytes, expected_size);

    let deadline = Instant::now() + Duration::from_secs(10);
    while store.active_fetches() > 0 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (origin, path) = store
        .resolve_local(rowid, Quality::Original)
        .unwrap()
        .expect("cached");
    assert_eq!(origin, Origin::Cache(Quality::Original));
    assert!(path.starts_with(paths.audio_cache()));
    assert_eq!(std::fs::metadata(path).unwrap().len(), expected_size);

    // A far seek on a not-yet-cached track uses an HTTP Range request.
    let second = tracks[1].clone();
    let reader = store.clone();
    let tail = tokio::task::spawn_blocking(move || {
        let handle = reader.open(second.rowid, Quality::Original).unwrap();
        let mut buf = vec![0u8; 1024];
        handle
            .read_at(second.size_bytes as u64 - 1024, &mut buf)
            .unwrap()
    })
    .await
    .unwrap();
    assert!(tail > 0);
}
