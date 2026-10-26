//! Library sync: initial sync, incremental additions, and the album-ID diff.

use lio_store::queries::{self, AlbumFilter, AlbumSort};
use lio_store::{Db, sync};
use lio_subsonic::{Client, Credentials, TlsConfig};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn envelope(key: &str, body: &str) -> String {
    format!(r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1","{key}":{body}}}}}"#)
}

fn album_json(
    id: &str,
    name: &str,
    artist: Option<(&str, &str)>,
    year: Option<i64>,
    created: &str,
) -> String {
    let artist_fields = match artist {
        Some((aid, aname)) => format!(
            r#","artist":"{aname}","artistId":"{aid}","artists":[{{"id":"{aid}","name":"{aname}"}}]"#
        ),
        None => String::new(),
    };
    let year = year.map(|y| format!(r#","year":{y}"#)).unwrap_or_default();
    format!(
        r#"{{"id":"{id}","name":"{name}","songCount":2,"duration":60,"created":"{created}","genres":[{{"name":"Rock"}}]{artist_fields}{year}}}"#
    )
}

async fn mount(server: &MockServer, endpoint: &str, body: String) {
    Mock::given(method("GET"))
        .and(path(format!("/rest/{endpoint}")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(server)
        .await;
}

async fn server(albums: &[String], last_scan: &str, newest: &[String]) -> MockServer {
    let server = MockServer::start().await;
    mount(
        &server,
        "getScanStatus",
        envelope(
            "scanStatus",
            &format!(r#"{{"scanning":false,"count":4,"lastScan":"{last_scan}"}}"#),
        ),
    )
    .await;
    mount(
        &server,
        "getArtists",
        envelope(
            "artists",
            r#"{"ignoredArticles":"The","index":[{"name":"B","artist":[{"id":"ar1","name":"The Band","albumCount":2},{"id":"ar2","name":"Björk","albumCount":1}]}]}"#,
        ),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/rest/getAlbumList2"))
        .and(query_param("type", "alphabeticalByName"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            envelope(
                "albumList2",
                &format!(r#"{{"album":[{}]}}"#, albums.join(",")),
            ),
            "application/json",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/getAlbumList2"))
        .and(query_param("type", "newest"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            envelope(
                "albumList2",
                &format!(r#"{{"album":[{}]}}"#, newest.join(",")),
            ),
            "application/json",
        ))
        .mount(&server)
        .await;
    mount(
        &server,
        "getPlaylists",
        envelope("playlists", r#"{"playlist":[{"id":"pl1","name":"Mix","songCount":1,"duration":30,"owner":"lio","changed":"2026-01-01T00:00:00Z"}]}"#),
    )
    .await;
    mount(
        &server,
        "getPlaylist",
        envelope("playlist", r#"{"id":"pl1","name":"Mix","songCount":1,"duration":30,"owner":"lio","changed":"2026-01-01T00:00:00Z","entry":[{"id":"s1","title":"Song One","albumId":"al1","duration":30}]}"#),
    )
    .await;
    mount(&server, "getStarred2", envelope("starred2", r#"{"album":[{"id":"al1","name":"Alpha","songCount":2,"duration":60,"starred":"2026-02-01T00:00:00Z"}]}"#)).await;
    mount(
        &server,
        "getAlbum",
        envelope("album", r#"{"id":"al1","name":"Alpha","songCount":2,"duration":60,"song":[{"id":"s1","title":"Song One","duration":30,"track":1},{"id":"s2","title":"Song Two","duration":30,"track":2,"artist":"Björk"}]}"#),
    )
    .await;
    server
}

fn client(server: &MockServer) -> Client {
    Client::new(
        &server.uri(),
        Credentials::token("lio", "pw"),
        TlsConfig::System,
    )
    .unwrap()
}

fn base_albums() -> Vec<String> {
    vec![
        album_json(
            "al1",
            "Alpha",
            Some(("ar1", "The Band")),
            Some(2001),
            "2026-01-01T00:00:00Z",
        ),
        album_json(
            "al2",
            "Beta",
            Some(("ar1", "The Band")),
            Some(1999),
            "2026-01-02T00:00:00Z",
        ),
        album_json(
            "al3",
            "Gamma",
            Some(("ar2", "Björk")),
            None,
            "2026-01-03T00:00:00Z",
        ),
        album_json("al4", "No Artist", None, None, "2026-01-04T00:00:00Z"),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_sync_fills_the_mirror() {
    let srv = server(&base_albums(), "2026-01-10T00:00:00Z", &[]).await;
    let db = Db::open_in_memory().unwrap();
    let report = sync::sync(&client(&srv), &db, "lio", false).await.unwrap();
    assert!(report.full);
    assert_eq!(report.albums_upserted, 4);
    assert_eq!(report.artists, 2);
    assert_eq!(report.playlists, 1);

    db.with(|c| {
        assert_eq!(queries::album_count(c)?, 4);
        let by_name = queries::album_rows(
            c,
            &queries::album_ids(c, AlbumSort::Name, &AlbumFilter::All, None)?,
        )?;
        assert_eq!(
            by_name.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Alpha", "Beta", "Gamma", "No Artist"]
        );
        let newest = queries::album_ids(c, AlbumSort::RecentlyAdded, &AlbumFilter::All, Some(1))?;
        assert_eq!(queries::album_rows(c, &newest)?[0].name, "No Artist");
        let by_year = queries::album_rows(
            c,
            &queries::album_ids(c, AlbumSort::Year, &AlbumFilter::All, None)?,
        )?;
        assert_eq!(
            by_year[0].name, "Alpha",
            "newest year first, unknown years last"
        );
        // Missing metadata is stored as NULL.
        let bare = by_name.iter().find(|a| a.name == "No Artist").unwrap();
        assert!(bare.artist_display.is_none() && bare.year.is_none());
        // Join tables.
        let rock =
            queries::album_ids(c, AlbumSort::Name, &AlbumFilter::Genre("Rock".into()), None)?;
        assert_eq!(rock.len(), 4);
        let band: i64 = c.query_row("SELECT rowid FROM artist WHERE id = 'ar1'", [], |r| {
            r.get(0)
        })?;
        assert_eq!(
            queries::album_ids(c, AlbumSort::Year, &AlbumFilter::Artist(band), None)?.len(),
            2
        );
        let band_sort: String =
            c.query_row("SELECT sort_name FROM artist WHERE id = 'ar1'", [], |r| {
                r.get(0)
            })?;
        assert_eq!(band_sort, "band", "leading article stripped");
        // Favourites and playlists.
        let starred = queries::album_ids(c, AlbumSort::Name, &AlbumFilter::Starred, None)?;
        assert_eq!(queries::album_rows(c, &starred)?[0].name, "Alpha");
        let playlists = queries::playlists(c)?;
        assert_eq!(playlists.len(), 1);
        assert_eq!(
            queries::playlist_track_rows(c, playlists[0].rowid)?[0].title,
            "Song One"
        );
        // FTS rows exist for artists and albums.
        let n: i64 = c.query_row(
            "SELECT COUNT(*) FROM search_fts WHERE kind IN ('artist', 'album')",
            [],
            |r| r.get(0),
        )?;
        assert!(n >= 6);
        Ok(())
    })
    .unwrap();

    // Background track fill.
    let synced = sync::sync_tracks(&client(&srv), &db, 10).await.unwrap();
    assert!(synced >= 1);
    db.with(|c| {
        let alpha = queries::album_row_by_id(c, "al1")?.unwrap();
        assert!(alpha.tracks_synced_at.is_some());
        assert_eq!(queries::album_track_rows(c, alpha.rowid)?.len(), 2);
        Ok(())
    })
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_sync_adds_new_albums_and_skips_when_unchanged() {
    let db = Db::open_in_memory().unwrap();
    let first = server(&base_albums(), "2026-01-10T00:00:00Z", &[]).await;
    sync::sync(&client(&first), &db, "lio", false)
        .await
        .unwrap();

    // Same lastScan: albums untouched.
    let report = sync::sync(&client(&first), &db, "lio", false)
        .await
        .unwrap();
    assert!(report.unchanged);
    assert!(!report.full);

    // New scan with one new album at the top of "newest".
    let new_album = album_json(
        "al5",
        "Delta",
        Some(("ar2", "Björk")),
        Some(2026),
        "2026-02-01T00:00:00Z",
    );
    let mut newest = vec![new_album.clone()];
    newest.extend(base_albums());
    let mut all = base_albums();
    all.push(new_album);
    let second = server(&all, "2026-02-01T00:00:00Z", &newest).await;
    let report = sync::sync(&client(&second), &db, "lio", false)
        .await
        .unwrap();
    assert!(!report.full, "incremental, not full");
    assert!(!report.unchanged);
    db.with(|c| {
        assert_eq!(queries::album_count(c)?, 5);
        assert!(queries::album_row_by_id(c, "al5")?.is_some());
        Ok(())
    })
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn album_diff_removes_deleted_albums() {
    let db = Db::open_in_memory().unwrap();
    let first = server(&base_albums(), "2026-01-10T00:00:00Z", &[]).await;
    sync::sync(&client(&first), &db, "lio", false)
        .await
        .unwrap();

    let remaining: Vec<String> = base_albums().into_iter().take(2).collect();
    let second = server(&remaining, "2026-01-20T00:00:00Z", &remaining).await;
    // A forced full sync runs the album-ID diff (otherwise it runs every 24 h).
    let report = sync::sync(&client(&second), &db, "lio", true)
        .await
        .unwrap();
    assert!(report.full);
    assert_eq!(report.albums_removed, 2);
    db.with(|c| {
        assert_eq!(queries::album_count(c)?, 2);
        assert!(queries::album_row_by_id(c, "al3")?.is_none());
        let fts: i64 = c.query_row(
            "SELECT COUNT(*) FROM search_fts WHERE kind = 'album'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(fts, 2, "search index pruned");
        Ok(())
    })
    .unwrap();
}
