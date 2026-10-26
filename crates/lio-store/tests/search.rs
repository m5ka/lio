//! Local search: diacritics, grouping, prefix ranking.

use lio_store::search::search_local;
use lio_store::{Db, library};
use lio_subsonic::SearchLimits;
use lio_subsonic::types::{Album, Artist, ArtistRef, Song};

fn seed(db: &Db) {
    db.with(|c| {
        for (id, name) in [
            ("a1", "Björk"),
            ("a2", "Bjorn Again"),
            ("a3", "The Sugarcubes"),
        ] {
            library::upsert_artist(
                c,
                &Artist {
                    id: id.into(),
                    name: name.into(),
                    album_count: 1,
                    ..Default::default()
                },
            )?;
        }
        let album = Album {
            id: "al1".into(),
            name: "Post".into(),
            artist: Some("Björk".into()),
            artists: vec![ArtistRef {
                id: "a1".into(),
                name: "Björk".into(),
            }],
            song: vec![
                Song {
                    id: "s1".into(),
                    title: "Army of Me".into(),
                    artist: Some("Björk".into()),
                    album: Some("Post".into()),
                    ..Default::default()
                },
                Song {
                    id: "s2".into(),
                    title: "Hyperballad".into(),
                    artist: Some("Björk".into()),
                    album: Some("Post".into()),
                    ..Default::default()
                },
                Song {
                    id: "s3".into(),
                    title: "Possibly Maybe".into(),
                    artist: Some("Björk".into()),
                    album: Some("Post".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        library::upsert_album_with_tracks(c, &album)?;
        let other = Album {
            id: "al2".into(),
            name: "Postcards from Bjorn".into(),
            artist: Some("Bjorn Again".into()),
            ..Default::default()
        };
        library::upsert_album(c, &other)?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn diacritics_are_ignored() {
    let db = Db::open_in_memory().unwrap();
    seed(&db);
    let r = db
        .with(|c| search_local(c, "bjork", SearchLimits::default()))
        .unwrap();
    let names: Vec<_> = r.artists.iter().map(|a| a.name.as_str()).collect();
    assert!(names.contains(&"Björk"), "{names:?}");
    assert!(
        !names.contains(&"Bjorn Again"),
        "whole-word prefix: 'bjork' ≠ 'bjorn'"
    );
}

#[test]
fn results_are_grouped_by_kind() {
    let db = Db::open_in_memory().unwrap();
    seed(&db);
    let r = db
        .with(|c| search_local(c, "post", SearchLimits::default()))
        .unwrap();
    assert!(r.albums.iter().any(|a| a.name == "Post"));
    assert_eq!(r.tracks.len(), 3, "tracks match via their album name");
    assert!(r.artists.is_empty());
    let r = db
        .with(|c| search_local(c, "army", SearchLimits::default()))
        .unwrap();
    assert_eq!(r.tracks.len(), 1);
    assert_eq!(r.tracks[0].title, "Army of Me");
}

#[test]
fn prefix_matches_rank_first() {
    let db = Db::open_in_memory().unwrap();
    seed(&db);
    let r = db
        .with(|c| search_local(c, "post", SearchLimits::default()))
        .unwrap();
    assert_eq!(
        r.albums[0].name, "Post",
        "exact prefix of the whole text first"
    );
    let r = db
        .with(|c| search_local(c, "bj", SearchLimits::default()))
        .unwrap();
    assert_eq!(r.artists.len(), 2, "short prefixes match both");
}

#[test]
fn limits_and_empty_queries() {
    let db = Db::open_in_memory().unwrap();
    seed(&db);
    let limits = SearchLimits {
        artists: 1,
        albums: 1,
        songs: 1,
    };
    let r = db.with(|c| search_local(c, "b", limits)).unwrap();
    assert!(r.artists.len() <= 1 && r.albums.len() <= 1 && r.tracks.len() <= 1);
    let r = db
        .with(|c| search_local(c, "   ", SearchLimits::default()))
        .unwrap();
    assert!(r.is_empty());
    let r = db
        .with(|c| search_local(c, "\"", SearchLimits::default()))
        .unwrap();
    assert!(r.is_empty());
}
