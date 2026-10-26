//! Downloads: status machine, shared files, sync, network policy.

use std::time::{Duration, Instant};

use lio_store::downloads::{self, Downloader, Event, PinKind, Policy, Status};
use lio_store::{AccountPaths, Db, Quality, library};
use lio_subsonic::types::{Album, Playlist, Song};
use lio_subsonic::{Client, Credentials, TlsConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Fixture {
    _tmp: tempfile::TempDir,
    _server: MockServer,
    db: Db,
    paths: AccountPaths,
    client: Client,
}

async fn fixture() -> Fixture {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/download"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![7u8; 4096], "audio/flac"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let paths = AccountPaths::with_roots(
        &server.uri(),
        "u",
        &tmp.path().join("d"),
        &tmp.path().join("c"),
    );
    paths.create_all().unwrap();
    let db = Db::open(&paths.db()).unwrap();
    let songs = |prefix: &str| {
        (1..=3)
            .map(|i| Song {
                id: format!("{prefix}{i}"),
                title: format!("T{i}"),
                size: 4096,
                ..Default::default()
            })
            .collect()
    };
    db.with(|c| {
        library::upsert_album_with_tracks(
            c,
            &Album {
                id: "al1".into(),
                name: "One".into(),
                song: songs("a"),
                ..Default::default()
            },
        )?;
        library::upsert_album_with_tracks(
            c,
            &Album {
                id: "al2".into(),
                name: "Two".into(),
                song: songs("b"),
                ..Default::default()
            },
        )?;
        // A playlist sharing track a1 with album one.
        let pl = Playlist {
            id: "pl1".into(),
            name: "Mix".into(),
            entry: vec![
                Song {
                    id: "a1".into(),
                    title: "T1".into(),
                    album_id: Some("al1".into()),
                    size: 4096,
                    ..Default::default()
                },
                Song {
                    id: "b2".into(),
                    title: "T2".into(),
                    album_id: Some("al2".into()),
                    size: 4096,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        library::replace_playlist_tracks(c, &pl, "u")?;
        Ok(())
    })
    .unwrap();
    let client = Client::new(
        &server.uri(),
        Credentials::token("u", "p"),
        TlsConfig::System,
    )
    .unwrap();
    Fixture {
        _tmp: tmp,
        _server: server,
        db,
        paths,
        client,
    }
}

async fn wait_status(db: &Db, rowid: i64, want: Status) -> downloads::Pin {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let pin = db.with(|c| downloads::pin_row(c, rowid)).unwrap().unwrap();
        if pin.status == want {
            return pin;
        }
        assert!(
            Instant::now() < deadline,
            "pin stuck in {:?}, wanted {want:?}",
            pin.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn start(f: &Fixture) -> (Downloader, async_channel::Receiver<Event>) {
    let (dl, events) = Downloader::new(f.db.clone(), f.paths.clone());
    dl.set_client(Some(f.client.clone()));
    tokio::spawn(dl.clone().run());
    (dl, events)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn album_downloads_to_complete() {
    let f = fixture().await;
    let (dl, _events) = start(&f);
    let rowid = dl
        .add(PinKind::Album, "al1", Quality::Original)
        .await
        .unwrap();
    let pin = wait_status(&f.db, rowid, Status::Complete).await;
    assert_eq!((pin.total_tracks, pin.done_tracks), (3, 3));
    assert!(pin.available_offline());
    assert_eq!(pin.bytes, 3 * 4096);
    // Files exist and the album counts as available offline.
    let album =
        f.db.with(|c| lio_store::queries::album_row_by_id(c, "al1"))
            .unwrap()
            .unwrap();
    assert_eq!(
        f.db.with(|c| downloads::album_offline_state(c, album.rowid))
            .unwrap(),
        downloads::OfflineState::Complete
    );
    assert_eq!(std::fs::read_dir(f.paths.downloads()).unwrap().count(), 3);
    dl.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_files_survive_removing_one_pin() {
    let f = fixture().await;
    let (dl, _events) = start(&f);
    let album = dl
        .add(PinKind::Album, "al1", Quality::Original)
        .await
        .unwrap();
    let playlist = dl
        .add(PinKind::Playlist, "pl1", Quality::Original)
        .await
        .unwrap();
    wait_status(&f.db, album, Status::Complete).await;
    wait_status(&f.db, playlist, Status::Complete).await;
    assert_eq!(
        std::fs::read_dir(f.paths.downloads()).unwrap().count(),
        4,
        "a1 is shared"
    );

    dl.remove(album).await.unwrap();
    let files =
        f.db.with(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM download_file", [], |r| {
                r.get::<_, i64>(0)
            })?)
        })
        .unwrap();
    assert_eq!(files, 2, "a1 stays (still in the playlist), a2/a3 go");
    assert_eq!(std::fs::read_dir(f.paths.downloads()).unwrap().count(), 2);

    dl.remove_all().await.unwrap();
    assert_eq!(std::fs::read_dir(f.paths.downloads()).unwrap().count(), 0);
    dl.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_resume_and_waiting_for_network() {
    let f = fixture().await;
    let (dl, _events) = start(&f);
    // Offline: the pin waits instead of failing.
    dl.set_policy(Policy {
        online: false,
        metered: false,
        allow_metered: false,
    });
    let rowid = dl
        .add(PinKind::Album, "al2", Quality::Original)
        .await
        .unwrap();
    wait_status(&f.db, rowid, Status::Waiting).await;
    // Metered without permission: still waiting.
    dl.set_policy(Policy {
        online: true,
        metered: true,
        allow_metered: false,
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        f.db.with(|c| downloads::pin_row(c, rowid))
            .unwrap()
            .unwrap()
            .status,
        Status::Waiting
    );
    // Paused pins are left alone even when allowed.
    dl.pause(rowid).await.unwrap();
    dl.set_policy(Policy {
        online: true,
        metered: false,
        allow_metered: false,
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        f.db.with(|c| downloads::pin_row(c, rowid))
            .unwrap()
            .unwrap()
            .status,
        Status::Paused
    );
    dl.resume(rowid).await.unwrap();
    wait_status(&f.db, rowid, Status::Complete).await;
    dl.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_follows_playlist_changes() {
    let f = fixture().await;
    let (dl, events) = start(&f);
    let rowid = dl
        .add(PinKind::Playlist, "pl1", Quality::Original)
        .await
        .unwrap();
    wait_status(&f.db, rowid, Status::Complete).await;

    // The server playlist changes: b2 removed, b3 added.
    f.db.with(|c| {
        let pl = Playlist {
            id: "pl1".into(),
            name: "Mix".into(),
            entry: vec![
                Song {
                    id: "a1".into(),
                    title: "T1".into(),
                    album_id: Some("al1".into()),
                    size: 4096,
                    ..Default::default()
                },
                Song {
                    id: "b3".into(),
                    title: "T3".into(),
                    album_id: Some("al2".into()),
                    size: 4096,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        library::replace_playlist_tracks(c, &pl, "u").map(drop)
    })
    .unwrap();
    dl.sync_pins().await.unwrap();
    let pin = wait_status(&f.db, rowid, Status::Complete).await;
    assert_eq!((pin.total_tracks, pin.done_tracks), (2, 2));
    let ids: Vec<String> = f
        .db
        .with(|c| {
            let mut stmt = c.prepare("SELECT t.id FROM download_file f JOIN track t ON t.rowid = f.track_rowid ORDER BY t.id")?;
            let ids = stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok(ids)
        })
        .unwrap();
    assert_eq!(ids, ["a1", "b3"], "b2's file was deleted, b3 downloaded");
    let mut saw_removed = false;
    while let Ok(e) = events.try_recv() {
        saw_removed |= matches!(e, Event::TracksRemoved { count: 1, .. });
    }
    assert!(saw_removed);
    dl.stop();
}

#[test]
fn status_strings_round_trip() {
    for s in [
        Status::Queued,
        Status::Downloading,
        Status::Paused,
        Status::Waiting,
        Status::Complete,
        Status::Syncing,
    ] {
        assert!(!s.as_str().is_empty());
    }
    assert!(Status::Waiting.wants_work());
    assert!(!Status::Paused.wants_work());
    assert!(!Status::Error("x".into()).wants_work());
    assert!(downloads::free_space(std::path::Path::new("/")).is_some());
}
