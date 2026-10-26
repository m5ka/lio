//! LRCLIB lookup against recorded-shape responses (lyrics text made up).

use lio_subsonic::lrclib::{Lrclib, LyricsQuery};
use wiremock::matchers::{header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

const QUERY: LyricsQuery<'static> = LyricsQuery {
    artist: "Lio Fixtures",
    title: "Sweep Part 3",
    album: Some("Gapless Sweep"),
    duration_s: Some(20),
};

#[tokio::test]
async fn exact_match_with_timed_lyrics() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .and(query_param("artist_name", "Lio Fixtures"))
        .and(query_param("track_name", "Sweep Part 3"))
        .and(query_param("album_name", "Gapless Sweep"))
        .and(query_param("duration", "20"))
        .and(header_regex(
            "user-agent",
            r"^Lio/\S+ \(https://github\.com/m5ka/lio\)$",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture("lrclib_get.json"), "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let record = Lrclib::with_base(&server.uri())
        .unwrap()
        .find(&QUERY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.id, 101);
    assert!(
        record
            .synced_lyrics
            .unwrap()
            .starts_with("[00:01.20] Halfway")
    );
}

#[tokio::test]
async fn not_found_falls_back_to_search() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(404).set_body_raw(
            r#"{"message":"Failed to find specified track","name":"TrackNotFound","statusCode":404}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("artist_name", "Lio Fixtures"))
        .and(query_param("track_name", "Sweep Part 3"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(fixture("lrclib_search.json"), "application/json"),
        )
        .mount(&server)
        .await;
    let record = Lrclib::with_base(&server.uri())
        .unwrap()
        .find(&QUERY)
        .await
        .unwrap()
        .unwrap();
    // 201 is too long (another version), 202 has no timed lyrics.
    assert_eq!(record.id, 203);
}

#[tokio::test]
async fn nothing_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
        .mount(&server)
        .await;
    let query = LyricsQuery {
        album: None,
        ..QUERY
    };
    let found = Lrclib::with_base(&server.uri())
        .unwrap()
        .find(&query)
        .await
        .unwrap();
    assert!(found.is_none());
}
