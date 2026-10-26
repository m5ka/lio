//! Against a real Navidrome: a play past the threshold counts exactly once, also
//! when it was made offline.
//!
//! "Offline" is simulated with an unreachable client (a closed port) instead of
//! `tests/test-server.sh pause`: a paused container only fails after the request
//! timeout, which would make the test slow without changing what it checks.

use lio_store::scrobble::{self, PlayTracker};
use lio_store::{Db, library};
use lio_subsonic::{AlbumListType, Client, Credentials, TlsConfig};

use crate::common;

async fn play_count(client: &Client, id: &str) -> i64 {
    client.get_song(id).await.unwrap().play_count.unwrap_or(0)
}

/// Simulates listening to a whole 20 s track.
fn listen(tracker: &mut PlayTracker, id: &str, duration_s: i64) -> Vec<scrobble::CompletedPlay> {
    tracker.start(id, duration_s, lio_store::db::now());
    (0..=duration_s as u64 * 4)
        .filter_map(|i| tracker.tick(i * 250, true))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs tests/test-server.sh up"]
async fn plays_count_exactly_once() {
    let srv = common::server();
    let client = Client::new(
        &srv.url,
        Credentials::token(&srv.user, &srv.password),
        TlsConfig::System,
    )
    .unwrap();
    let albums = client
        .get_all_albums(&AlbumListType::AlphabeticalByName)
        .await
        .unwrap();
    let album = client
        .get_album(&albums.iter().find(|a| a.name == "ReplayGain").unwrap().id)
        .await
        .unwrap();
    let song = album.song[0].clone();
    let db = Db::open_in_memory().unwrap();
    db.with(|c| library::upsert_album_with_tracks(c, &album).map(drop))
        .unwrap();

    // Online play.
    let before = play_count(&client, &song.id).await;
    let mut tracker = PlayTracker::new();
    let plays = listen(&mut tracker, &song.id, song.duration);
    assert_eq!(plays.len(), 1);
    db.with(|c| scrobble::enqueue(c, &plays[0])).unwrap();
    assert_eq!(scrobble::submit_pending(&client, &db).await.unwrap(), 1);
    assert_eq!(
        scrobble::submit_pending(&client, &db).await.unwrap(),
        0,
        "nothing sent twice"
    );
    assert_eq!(play_count(&client, &song.id).await, before + 1);

    // Offline play: queued, delivered once the server is reachable again.
    let offline = Client::new(
        "http://127.0.0.1:9",
        Credentials::token(&srv.user, &srv.password),
        TlsConfig::System,
    )
    .unwrap();
    let plays = listen(&mut tracker, &song.id, song.duration);
    db.with(|c| scrobble::enqueue(c, &plays[0])).unwrap();
    assert!(scrobble::submit_pending(&offline, &db).await.is_err());
    assert_eq!(
        db.with(|c| scrobble::pending(c, 10)).unwrap().len(),
        1,
        "kept for later"
    );
    assert_eq!(play_count(&client, &song.id).await, before + 1);

    assert_eq!(scrobble::submit_pending(&client, &db).await.unwrap(), 1);
    assert_eq!(scrobble::submit_pending(&client, &db).await.unwrap(), 0);
    assert_eq!(
        play_count(&client, &song.id).await,
        before + 2,
        "the offline play arrived exactly once"
    );
}
