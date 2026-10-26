//! Contract tests for the Subsonic client, against recorded Navidrome responses.

use std::collections::HashMap;

use lio_subsonic::{Client, Credentials, Error, TlsConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

async fn server_with(endpoint: &str, fixture_name: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/rest/{endpoint}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture(fixture_name), "application/json"),
        )
        .mount(&server)
        .await;
    server
}

fn token_client(server: &MockServer) -> Client {
    Client::new(
        &server.uri(),
        Credentials::token("lio", "lio-test"),
        TlsConfig::System,
    )
    .unwrap()
}

async fn recorded_queries(server: &MockServer) -> Vec<Vec<(String, String)>> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| {
            r.url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .collect()
}

/// Invariants 1 and 2: no `p=`, and `v`, `c=lio`, `f=json` plus (`u`,`t`,`s`) or `apiKey`.
fn assert_request_shape(query: &[(String, String)], api_key: bool) {
    let map: HashMap<_, _> = query.iter().cloned().collect();
    assert!(!map.contains_key("p"), "plain password sent: {query:?}");
    assert!(
        query.iter().all(|(_, v)| v != "lio-test"),
        "password value leaked: {query:?}"
    );
    assert_eq!(map.get("v").map(String::as_str), Some("1.16.1"));
    assert_eq!(map.get("c").map(String::as_str), Some("lio"));
    assert_eq!(map.get("f").map(String::as_str), Some("json"));
    if api_key {
        assert!(map.contains_key("apiKey"));
        assert!(!map.contains_key("t") && !map.contains_key("s"));
    } else {
        assert_eq!(map.get("u").map(String::as_str), Some("lio"));
        let (t, s) = (&map["t"], &map["s"]);
        assert_eq!(
            *t,
            lio_subsonic::token("lio-test", s),
            "t != md5(password + s)"
        );
    }
}

#[tokio::test]
async fn ping_request_shape_token_auth() {
    let server = server_with("ping", "ping.json").await;
    let client = token_client(&server);
    let info = client.ping().await.unwrap();
    assert_eq!(info.server_type, "navidrome");
    assert!(info.open_subsonic);
    assert!(info.is_fully_compatible());

    for q in recorded_queries(&server).await {
        assert_request_shape(&q, false);
    }
}

#[tokio::test]
async fn ping_request_shape_api_key() {
    let server = server_with("ping", "ping.json").await;
    let client = Client::new(
        &server.uri(),
        Credentials::api_key("lio", "secret-key"),
        TlsConfig::System,
    )
    .unwrap();
    client.ping().await.unwrap();
    for q in recorded_queries(&server).await {
        assert_request_shape(&q, true);
    }
}

/// Invariant 3: the salt differs between consecutive requests.
#[tokio::test]
async fn salt_differs_between_requests() {
    let server = server_with("ping", "ping.json").await;
    let client = token_client(&server);
    client.ping().await.unwrap();
    client.ping().await.unwrap();
    client.ping().await.unwrap();
    let salts: Vec<String> = recorded_queries(&server)
        .await
        .iter()
        .map(|q| q.iter().find(|(k, _)| k == "s").unwrap().1.clone())
        .collect();
    assert_eq!(salts.len(), 3);
    assert_ne!(salts[0], salts[1]);
    assert_ne!(salts[1], salts[2]);
    assert_ne!(salts[0], salts[2]);
}

#[tokio::test]
async fn extensions_and_user() {
    let server = server_with(
        "getOpenSubsonicExtensions",
        "getOpenSubsonicExtensions.json",
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/rest/getUser"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("getUser.json")))
        .mount(&server)
        .await;
    let client = token_client(&server);
    let exts = client.get_open_subsonic_extensions().await.unwrap();
    assert!(exts.iter().any(|e| e.name == "transcodeOffset"));
    let user = client.get_user("lio").await.unwrap();
    assert_eq!(user.username, "lio");
    assert!(user.download_role);
    // Navidrome reports playlistRole=false even though every user can manage their own
    // playlists, so Lio must not gate playlist editing on it.
    assert!(!user.playlist_role);
}

#[tokio::test]
async fn error_codes_map_per_contract() {
    for (fixture_name, check) in [
        (
            "error_40.json",
            (|e: &Error| matches!(e, Error::BadCredentials)) as fn(&Error) -> bool,
        ),
        ("error_50.json", |e| matches!(e, Error::NotAuthorized)),
        ("error_70.json", |e| matches!(e, Error::NotFound)),
    ] {
        let server = server_with("ping", fixture_name).await;
        let err = token_client(&server).ping().await.unwrap_err();
        assert!(check(&err), "{fixture_name}: got {err:?}");
    }

    // Codes without a fixture: synthesise envelopes.
    for (code, check) in [
        (
            41,
            (|e: &Error| matches!(e, Error::BadCredentials)) as fn(&Error) -> bool,
        ),
        (44, |e| matches!(e, Error::BadCredentials)),
        (20, |e| matches!(e, Error::ServerTooOld)),
        (30, |e| matches!(e, Error::ServerTooOld)),
        (10, |e| matches!(e, Error::Server { code: 10, .. })),
    ] {
        let server = MockServer::start().await;
        let body = format!(
            r#"{{"subsonic-response":{{"status":"failed","version":"1.16.1","error":{{"code":{code},"message":"m"}}}}}}"#
        );
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let err = token_client(&server).ping().await.unwrap_err();
        assert!(check(&err), "code {code}: got {err:?}");
    }
}

#[tokio::test]
async fn non_subsonic_response_is_incompatible() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>hello</html>"))
        .mount(&server)
        .await;
    let err = token_client(&server).ping().await.unwrap_err();
    assert!(matches!(err, Error::Incompatible), "{err:?}");

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let err = token_client(&server).ping().await.unwrap_err();
    assert!(matches!(err, Error::Incompatible), "{err:?}");
}

#[tokio::test]
async fn unreachable_server() {
    // Port 9 (discard) is closed on test machines.
    let client = Client::new(
        "http://127.0.0.1:9",
        Credentials::token("u", "p"),
        TlsConfig::System,
    )
    .unwrap();
    let err = client.ping().await.unwrap_err();
    assert!(matches!(err, Error::Unreachable), "{err:?}");
}

/// Invariant 6: `original` streams use `format=raw` + `estimateContentLength=true`,
/// and a `Range` request is answered with `206 Partial Content`.
#[tokio::test]
async fn stream_original_and_range() {
    use wiremock::matchers::header;

    let server = MockServer::start().await;
    let body: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .and(header("range", "bytes=4000-"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-type", "audio/flac")
                .insert_header("content-range", "bytes 4000-9999/10000")
                .set_body_bytes(body[4000..].to_vec()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "audio/flac")
                .set_body_bytes(body.clone()),
        )
        .mount(&server)
        .await;

    let client = token_client(&server);
    let url = client.stream_url("song1", "raw", None, None);
    let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q.get("format").map(String::as_str), Some("raw"));
    assert_eq!(
        q.get("estimateContentLength").map(String::as_str),
        Some("true")
    );
    assert_eq!(q.get("id").map(String::as_str), Some("song1"));
    assert!(!q.contains_key("maxBitRate"));

    let full = client.open_stream(url.clone(), 0).await.unwrap();
    assert!(!full.partial);
    assert_eq!(full.total, Some(10_000));
    assert_eq!(full.response.bytes().await.unwrap().len(), 10_000);

    let part = client.open_stream(url, 4000).await.unwrap();
    assert!(part.partial);
    assert_eq!(part.start, 4000);
    assert_eq!(part.total, Some(10_000));
    assert_eq!(part.response.bytes().await.unwrap().as_ref(), &body[4000..]);

    for q in recorded_queries(&server).await {
        assert_request_shape(&q, false);
    }
}

#[tokio::test]
async fn transcoded_stream_url() {
    let server = MockServer::start().await;
    let client = token_client(&server);
    let url = client.stream_url("s", "opus", Some(128), Some(42));
    let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["format"], "opus");
    assert_eq!(q["maxBitRate"], "128");
    assert_eq!(q["timeOffset"], "42");
    assert_eq!(q["estimateContentLength"], "true");
}

#[tokio::test]
async fn stream_error_envelope_is_mapped() {
    let server = server_with("stream", "error_70.json").await;
    let client = token_client(&server);
    let err = client
        .open_stream(client.stream_url("x", "raw", None, None), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound), "{err:?}");
}

/// Invariant 5: unknown fields are ignored and missing optional fields tolerated.
#[tokio::test]
async fn album_list_and_album_parsing() {
    let server = server_with("getAlbumList2", "getAlbumList2.json").await;
    Mock::given(method("GET"))
        .and(path("/rest/getAlbum"))
        .and(wiremock::matchers::query_param("id", "minimal1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("album_minimal.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/getAlbum"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("getAlbum.json")))
        .mount(&server)
        .await;
    let client = token_client(&server);

    let albums = client
        .get_album_list2(&lio_subsonic::AlbumListType::AlphabeticalByName, 500, 0)
        .await
        .unwrap();
    assert!(albums.len() >= 5, "fixture has five albums");
    let bare = albums
        .iter()
        .find(|a| a.name == "Bare Album")
        .expect("bare album present");
    assert!(bare.year.is_none(), "year 0 must map to None");
    let sweep = albums.iter().find(|a| a.name == "Gapless Sweep").unwrap();
    assert_eq!(sweep.year, Some(2026));
    assert_eq!(sweep.song_count, 5);
    assert!(sweep.created_at().is_some());
    assert!(sweep.cover_art.is_some());
    assert_eq!(sweep.genre_names(), vec!["Test Tones"]);

    let album = client.get_album("any").await.unwrap();
    assert_eq!(album.song.len(), 5);
    let first = &album.song[0];
    assert_eq!(first.track, Some(1));
    assert_eq!(first.suffix.as_deref(), Some("flac"));
    assert_eq!(first.duration, 20);
    assert_eq!(first.artist_display(), Some("Lio Fixtures"));
    assert!(
        first.starred_at().is_some(),
        "fixture starred the first song"
    );

    let minimal = client.get_album("minimal1").await.unwrap();
    assert_eq!(minimal.name, "Minimal");
    assert!(minimal.artist.is_none() && minimal.year.is_none() && minimal.cover_art.is_none());
    assert_eq!(minimal.song[0].title, "Only Title");
    assert!(minimal.song[0].artist_display().is_none());
    assert!(minimal.song[0].replay_gain.is_none());
}

/// Invariant 4: `getAlbumList2` pages with increasing `offset` until a short page.
#[tokio::test]
async fn album_paging_until_short_page() {
    use wiremock::matchers::query_param;

    let server = MockServer::start().await;
    let page = |n: usize, start: usize| {
        let albums: Vec<String> = (start..start + n)
            .map(|i| format!(r#"{{"id":"a{i}","name":"Album {i}","songCount":1,"duration":1}}"#))
            .collect();
        format!(
            r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1","albumList2":{{"album":[{}]}}}}}}"#,
            albums.join(",")
        )
    };
    for (offset, n) in [(0, 500), (500, 500), (1000, 123)] {
        Mock::given(method("GET"))
            .and(path("/rest/getAlbumList2"))
            .and(query_param("offset", offset.to_string()))
            .and(query_param("size", "500"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(page(n, offset), "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let albums = token_client(&server)
        .get_all_albums(&lio_subsonic::AlbumListType::AlphabeticalByName)
        .await
        .unwrap();
    assert_eq!(albums.len(), 1123);
    assert_eq!(albums[1122].id, "a1122");
    let offsets: Vec<u32> = recorded_queries(&server)
        .await
        .iter()
        .map(|q| {
            q.iter()
                .find(|(k, _)| k == "offset")
                .unwrap()
                .1
                .parse()
                .unwrap()
        })
        .collect();
    assert_eq!(offsets, vec![0, 500, 1000]);
}

#[tokio::test]
async fn browse_fixtures_parse() {
    let server = MockServer::start().await;
    for (endpoint, file) in [
        ("getArtists", "getArtists.json"),
        ("getArtist", "getArtist.json"),
        ("getGenres", "getGenres.json"),
        ("getSong", "getSong.json"),
        ("getScanStatus", "getScanStatus.json"),
        ("getStarred2", "getStarred2.json"),
        ("getPlaylists", "getPlaylists.json"),
        ("getPlaylist", "getPlaylist.json"),
        ("search3", "search3.json"),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/rest/{endpoint}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(fixture(file), "application/json"),
            )
            .mount(&server)
            .await;
    }
    let client = token_client(&server);

    let index = client.get_artists().await.unwrap();
    let names: Vec<_> = index.artists().map(|a| a.name.as_str()).collect();
    assert!(names.contains(&"Lio Fixtures"), "{names:?}");
    assert!(names.contains(&"Ünïcödé Ärtist"));
    assert!(index.artists().any(|a| a.starred_at().is_some()));

    let artist = client.get_artist("x").await.unwrap();
    assert_eq!(artist.name, "Lio Fixtures");
    assert_eq!(artist.album.len(), 3);

    let genres = client.get_genres().await.unwrap();
    assert!(
        genres
            .iter()
            .any(|g| g.value == "Électronique" && g.album_count == 1)
    );

    let song = client.get_song("x").await.unwrap();
    assert_eq!(song.title, "Sweep Part 1");
    assert_eq!(song.sampling_rate, Some(44100));

    let scan = client.get_scan_status().await.unwrap();
    assert!(!scan.scanning);
    assert!(scan.last_scan_at().is_some());

    let starred = client.get_starred2().await.unwrap();
    assert_eq!(starred.artist.len(), 1);
    assert_eq!(starred.album.len(), 1);
    assert_eq!(starred.song.len(), 1);

    let playlists = client.get_playlists().await.unwrap();
    assert_eq!(playlists.len(), 1);
    assert!(!playlists[0].is_readonly_for("lio"));
    assert!(playlists[0].changed_at().is_some());

    let playlist = client.get_playlist("x").await.unwrap();
    assert_eq!(playlist.entry.len(), 2);

    let results = client
        .search3("sweep", lio_subsonic::SearchLimits::default())
        .await
        .unwrap();
    assert!(results.song.len() >= 5);
    assert!(results.album.iter().any(|a| a.name == "Gapless Sweep"));

    let q: HashMap<_, _> = recorded_queries(&server)
        .await
        .into_iter()
        .find(|q| q.iter().any(|(k, v)| k == "query" && v == "sweep"))
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(q["artistCount"], "20");
    assert_eq!(q["albumCount"], "20");
    assert_eq!(q["songCount"], "50");
}

/// Annotation and playlist request shapes.
#[tokio::test]
async fn annotation_and_playlist_requests() {
    let server = MockServer::start().await;
    let ok = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
    let created = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlist":{"id":"pl9","name":"New","songCount":2}}}"#;
    Mock::given(method("GET"))
        .and(path("/rest/createPlaylist"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(created, "application/json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ok, "application/json"))
        .mount(&server)
        .await;
    let client = token_client(&server);
    use lio_subsonic::StarTarget;

    client.star(&StarTarget::Song("s1".into())).await.unwrap();
    client.star(&StarTarget::Album("al1".into())).await.unwrap();
    client
        .unstar(&StarTarget::Artist("ar1".into()))
        .await
        .unwrap();
    client.set_rating("s1", 4).await.unwrap();
    client.set_rating("s1", 0).await.unwrap();
    let pl = client
        .create_playlist("New", &["s1".into(), "s2".into()])
        .await
        .unwrap();
    assert_eq!(pl.id, "pl9");
    client
        .replace_playlist_tracks("pl9", &["s2".into(), "s1".into()])
        .await
        .unwrap();
    client
        .update_playlist("pl9", Some("Renamed"), &["s3".into()], &[0])
        .await
        .unwrap();
    client.delete_playlist("pl9").await.unwrap();
    client.scrobble("s1", false, None).await.unwrap();
    client
        .scrobble("s1", true, Some(1_700_000_000_000))
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let calls: Vec<(String, Vec<(String, String)>)> = requests
        .iter()
        .map(|r| {
            let endpoint = r.url.path().rsplit('/').next().unwrap().to_owned();
            let params = r
                .url
                .query_pairs()
                .filter(|(k, _)| !matches!(k.as_ref(), "u" | "t" | "s" | "v" | "c" | "f"))
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            (endpoint, params)
        })
        .collect();
    let p = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<Vec<_>>()
    };
    assert_eq!(calls[0], ("star".into(), p(&[("id", "s1")])));
    assert_eq!(calls[1], ("star".into(), p(&[("albumId", "al1")])));
    assert_eq!(calls[2], ("unstar".into(), p(&[("artistId", "ar1")])));
    assert_eq!(
        calls[3],
        ("setRating".into(), p(&[("id", "s1"), ("rating", "4")]))
    );
    assert_eq!(
        calls[4],
        ("setRating".into(), p(&[("id", "s1"), ("rating", "0")]))
    );
    assert_eq!(
        calls[5],
        (
            "createPlaylist".into(),
            p(&[("name", "New"), ("songId", "s1"), ("songId", "s2")])
        )
    );
    assert_eq!(
        calls[6],
        (
            "createPlaylist".into(),
            p(&[("playlistId", "pl9"), ("songId", "s2"), ("songId", "s1")])
        ),
        "reorder replaces the content in order"
    );
    assert_eq!(
        calls[7],
        (
            "updatePlaylist".into(),
            p(&[
                ("playlistId", "pl9"),
                ("name", "Renamed"),
                ("songIdToAdd", "s3"),
                ("songIndexToRemove", "0")
            ])
        )
    );
    assert_eq!(calls[8], ("deletePlaylist".into(), p(&[("id", "pl9")])));
    assert_eq!(
        calls[9],
        (
            "scrobble".into(),
            p(&[("id", "s1"), ("submission", "false")])
        )
    );
    assert_eq!(
        calls[10],
        (
            "scrobble".into(),
            p(&[
                ("id", "s1"),
                ("submission", "true"),
                ("time", "1700000000000")
            ])
        )
    );
    for q in recorded_queries(&server).await {
        assert_request_shape(&q, false);
    }
}

#[tokio::test]
async fn lyrics_structured_and_classic() {
    let server = server_with("getLyricsBySongId", "getLyricsBySongId.json").await;
    Mock::given(method("GET"))
        .and(path("/rest/getLyrics"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("getLyrics.json")))
        .mount(&server)
        .await;
    let client = token_client(&server);

    let sets = client.get_lyrics_by_song_id("s1").await.unwrap();
    assert_eq!(sets.len(), 1);
    let synced = &sets[0];
    assert!(synced.synced);
    assert_eq!(synced.line.len(), 5);
    assert_eq!(synced.line[0].start, Some(500));
    assert_eq!(synced.line[0].value, "The sweep begins low and slow");

    let plain = client
        .get_lyrics("Lio Fixtures", "Sweep Part 2")
        .await
        .unwrap()
        .unwrap();
    assert!(plain.starts_with("Plain lyrics without timestamps\n"));

    let queries = recorded_queries(&server).await;
    let has = |q: &Vec<(String, String)>, k: &str, v: &str| q.iter().any(|(a, b)| a == k && b == v);
    assert!(has(&queries[0], "id", "s1"));
    assert!(
        has(&queries[1], "artist", "Lio Fixtures") && has(&queries[1], "title", "Sweep Part 2")
    );
}

#[tokio::test]
async fn lyrics_empty_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getLyricsBySongId"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","openSubsonic":true,"lyricsList":{}}}"#,
        ))
        .mount(&server)
        .await;
    let client = token_client(&server);
    assert!(client.get_lyrics_by_song_id("s3").await.unwrap().is_empty());
}
