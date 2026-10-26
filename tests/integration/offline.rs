//! Against a real Navidrome: downloads play with the server gone.

use std::time::{Duration, Instant};

use lio_store::downloads::{self, Downloader, PinKind, Status};
use lio_store::{AccountPaths, Db, Origin, Quality, TrackStore, queries, sync};
use lio_subsonic::{AlbumListType, Client, Credentials, TlsConfig};

use crate::common;

async fn wait_complete(db: &Db, rowid: i64) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let pin = db.with(|c| downloads::pin_row(c, rowid)).unwrap().unwrap();
        if pin.status == Status::Complete {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "download stuck in {:?}",
            pin.status
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs tests/test-server.sh up"]
async fn downloads_play_offline() {
    let srv = common::server();
    let client = Client::new(
        &srv.url,
        Credentials::token(&srv.user, &srv.password),
        TlsConfig::System,
    )
    .unwrap();
    let xdg = common::temp_xdg();
    let paths = AccountPaths::with_roots(&srv.url, &srv.user, &xdg.data_home(), &xdg.cache_home());
    paths.create_all().unwrap();
    let db = Db::open(&paths.db()).unwrap();
    sync::sync(&client, &db, &srv.user, false).await.unwrap();
    while sync::sync_tracks(&client, &db, 50).await.unwrap() > 0 {}

    let albums = client
        .get_all_albums(&AlbumListType::AlphabeticalByName)
        .await
        .unwrap();
    let mix = albums
        .iter()
        .find(|a| a.name == "Format Mix")
        .unwrap()
        .id
        .clone();
    let playlist_id = client
        .get_playlists()
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.name == "Fixture Playlist")
        .expect("the fixture playlist (2 songs)")
        .id;

    // Download an album and a playlist.
    let (dl, _events) = Downloader::new(db.clone(), paths.clone());
    dl.set_client(Some(client.clone()));
    tokio::spawn(dl.clone().run());
    let album_pin = dl
        .add(PinKind::Album, &mix, Quality::Original)
        .await
        .unwrap();
    let playlist_pin = dl
        .add(PinKind::Playlist, &playlist_id, Quality::Original)
        .await
        .unwrap();
    wait_complete(&db, album_pin).await;
    wait_complete(&db, playlist_pin).await;
    dl.stop();

    // Cache one more track at original quality while online.
    let store = TrackStore::new(db.clone(), paths.clone(), tokio::runtime::Handle::current());
    store.set_client(Some(client.clone()));
    let rg_album = albums
        .iter()
        .find(|a| a.name == "ReplayGain")
        .unwrap()
        .id
        .clone();
    let rg_rowid = db
        .with(|c| queries::album_row_by_id(c, &rg_album))
        .unwrap()
        .unwrap()
        .rowid;
    let rg_tracks = db.with(|c| queries::album_track_rows(c, rg_rowid)).unwrap();
    let cached = rg_tracks[0].rowid;
    let s = store.clone();
    tokio::task::spawn_blocking(move || s.prefetch(cached, Quality::Original).unwrap())
        .await
        .unwrap();
    while store.active_fetches() > 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The server goes away.
    let gone = Client::new(
        "http://127.0.0.1:9",
        Credentials::token(&srv.user, &srv.password),
        TlsConfig::System,
    )
    .unwrap();
    store.set_client(Some(gone));
    store.set_online(false);

    let downloaded: Vec<i64> = db
        .with(|c| {
            let mut stmt = c.prepare("SELECT track_rowid FROM download_track")?;
            let ids = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            Ok(ids)
        })
        .unwrap();
    assert!(downloaded.len() >= 5, "album (3) + playlist (2) tracks");
    let s = store.clone();
    let (uncached, cached_quality_mismatch) = (rg_tracks[1].rowid, cached);
    let checks = tokio::task::spawn_blocking(move || {
        for rowid in &downloaded {
            let handle = s
                .open(*rowid, Quality::Original)
                .expect("downloaded track opens offline");
            assert!(
                matches!(handle.origin(), Origin::Download(_)),
                "{:?}",
                handle.origin()
            );
            let mut buf = [0u8; 4];
            assert_eq!(handle.read_at(0, &mut buf).unwrap(), 4);
        }
        let err = s
            .open(uncached, Quality::Original)
            .err()
            .expect("not available offline");
        assert!(
            matches!(err, lio_store::Error::UnavailableOffline),
            "{err:?}"
        );
        // Cached only at `original`, requested at `opus-128`: resolution step 3.
        let started = Instant::now();
        let handle = s
            .open(cached_quality_mismatch, Quality::Opus128)
            .expect("cached copy used");
        let elapsed = started.elapsed();
        assert_eq!(handle.origin(), Origin::Cache(Quality::Original));
        elapsed
    })
    .await
    .unwrap();
    assert!(
        checks < Duration::from_millis(200),
        "cached track opened in {checks:?}"
    );
}
