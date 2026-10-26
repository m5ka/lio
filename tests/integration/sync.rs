//! Against a real Navidrome: a full sync matches the server's counts.

use lio_store::{Db, queries, sync};
use lio_subsonic::{AlbumListType, Client, Credentials, TlsConfig};

use crate::common;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs tests/test-server.sh up"]
async fn full_sync_matches_server() {
    let srv = common::server();
    let client = Client::new(
        &srv.url,
        Credentials::token(&srv.user, &srv.password),
        TlsConfig::System,
    )
    .unwrap();
    let db = Db::open_in_memory().unwrap();

    let report = sync::sync(&client, &db, &srv.user, false)
        .await
        .expect("sync");
    assert!(report.full);

    let server_albums = client
        .get_all_albums(&AlbumListType::AlphabeticalByName)
        .await
        .unwrap();
    let server_tracks: i64 = server_albums.iter().map(|a| a.song_count).sum();
    assert_eq!(
        db.with(|c| queries::album_count(c)).unwrap() as usize,
        server_albums.len()
    );

    // Fill every album's track list, then compare track counts.
    while sync::sync_tracks(&client, &db, 50).await.unwrap() > 0 {}
    assert!(!db.with(|c| queries::tracks_sync_incomplete(c)).unwrap());
    let local_tracks: i64 = db
        .with(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM track WHERE album_rowid IS NOT NULL",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(local_tracks, server_tracks);

    let playlists = client.get_playlists().await.unwrap();
    assert_eq!(
        db.with(|c| queries::playlists(c)).unwrap().len(),
        playlists.len()
    );

    // A second sync with no server changes skips the album listing.
    let again = sync::sync(&client, &db, &srv.user, false).await.unwrap();
    assert!(again.unchanged);
}
