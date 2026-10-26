//! Scrobbling: threshold, once per play, ordering, back-off, purge.

use lio_store::Db;
use lio_store::scrobble::{self, CompletedPlay, PlayTracker, threshold_ms};
use lio_subsonic::{Client, Credentials, TlsConfig};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Simulates playback: `secs` of playing from `from_s`, ticking every 250 ms.
fn play(t: &mut PlayTracker, from_s: u64, secs: u64) -> Vec<CompletedPlay> {
    let mut out = Vec::new();
    for step in 0..=(secs * 4) {
        if let Some(p) = t.tick(from_s * 1000 + step * 250, true) {
            out.push(p);
        }
    }
    out
}

#[test]
fn thresholds() {
    assert_eq!(threshold_ms(20), 10_000, "half of a short track");
    assert_eq!(threshold_ms(600), 240_000, "capped at 4 minutes");
    assert_eq!(threshold_ms(0), 0);
}

#[test]
fn counts_once_after_half_the_track() {
    let mut t = PlayTracker::new();
    t.start("s1", 20, 1000);
    assert!(
        play(&mut t, 0, 9).is_empty(),
        "9 s of a 20 s track isn't enough"
    );
    let done = play(&mut t, 9, 5);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].track_id, "s1");
    assert_eq!(done[0].played_at, 1000);
    assert!(play(&mut t, 14, 6).is_empty(), "never twice for one play");
}

#[test]
fn seeks_and_pauses_do_not_count() {
    let mut t = PlayTracker::new();
    t.start("s1", 600, 0);
    assert!(play(&mut t, 0, 10).is_empty());
    // Seek far ahead: the jump is not listening time.
    assert!(t.tick(500_000, true).is_none());
    // Paused ticks don't accumulate either.
    t.pause();
    for i in 0..1000 {
        assert!(t.tick(500_000 + i, false).is_none());
    }
    // 230 more seconds of listening crosses 240 s total.
    let done = play(&mut t, 500, 231);
    assert_eq!(done.len(), 1);
}

#[test]
fn replay_is_a_new_play() {
    let mut t = PlayTracker::new();
    t.start("s1", 20, 0);
    let first = play(&mut t, 0, 20);
    t.start("s1", 20, 20);
    let second = play(&mut t, 0, 20);
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].play_id, second[0].play_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submits_in_order_backs_off_and_purges() {
    let db = Db::open_in_memory().unwrap();
    db.with(|c| {
        for (id, at) in [("s2", 200), ("s1", 100), ("s3", 300)] {
            scrobble::enqueue(
                c,
                &CompletedPlay {
                    play_id: uuid::Uuid::new_v4(),
                    track_id: id.into(),
                    played_at: at,
                },
            )?;
        }
        Ok(())
    })
    .unwrap();

    // Server fails on s2: s1 is delivered, s2 counts an attempt, s3 waits.
    let server = MockServer::start().await;
    let ok = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
    Mock::given(method("GET"))
        .and(path("/rest/scrobble"))
        .and(query_param("id", "s2"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/scrobble"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ok, "application/json"))
        .mount(&server)
        .await;
    let client = Client::new(
        &server.uri(),
        Credentials::token("u", "p"),
        TlsConfig::System,
    )
    .unwrap();
    assert!(scrobble::submit_pending(&client, &db).await.is_err());
    let pending = db.with(|c| scrobble::pending(c, 10)).unwrap();
    assert_eq!(
        pending.iter().map(|p| p.1.as_str()).collect::<Vec<_>>(),
        ["s2", "s3"]
    );
    let attempts: i64 = db
        .with(|c| {
            Ok(c.query_row(
                "SELECT attempts FROM scrobble WHERE track_id = 's2'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(attempts, 1);
    let times: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter_map(|r| {
            r.url
                .query_pairs()
                .find(|(k, _)| k == "time")
                .map(|(_, v)| v.into_owned())
        })
        .collect();
    assert_eq!(
        times[0], "100000",
        "played_at sent in milliseconds, oldest first"
    );

    // Server healthy again: the rest goes through, each exactly once.
    let healthy = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ok, "application/json"))
        .mount(&healthy)
        .await;
    let client = Client::new(
        &healthy.uri(),
        Credentials::token("u", "p"),
        TlsConfig::System,
    )
    .unwrap();
    assert_eq!(scrobble::submit_pending(&client, &db).await.unwrap(), 2);
    assert_eq!(scrobble::submit_pending(&client, &db).await.unwrap(), 0);
    assert!(db.with(|c| scrobble::pending(c, 10)).unwrap().is_empty());

    // Purge after 7 days.
    let now = lio_store::db::now();
    assert_eq!(db.with(|c| scrobble::purge(c, now)).unwrap(), 0);
    assert_eq!(
        db.with(|c| scrobble::purge(c, now + 8 * 24 * 3600))
            .unwrap(),
        3
    );
}
