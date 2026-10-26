//! Play queue: insertion, reordering, shuffle, repeat, persistence.

use lio_store::queue::{Origin, Queue, QueueEntry, Repeat};
use lio_store::{Db, TrackRow, library};
use lio_subsonic::types::{Album, Song};

fn row(rowid: i64) -> TrackRow {
    TrackRow {
        rowid,
        id: format!("t{rowid}"),
        title: format!("Track {rowid}"),
        ..Default::default()
    }
}

fn entries(ids: &[i64]) -> Vec<QueueEntry> {
    ids.iter()
        .map(|&i| QueueEntry::new(row(i), Origin::default()))
        .collect()
}

fn ids(q: &Queue) -> Vec<i64> {
    q.items().iter().map(|e| e.track.rowid).collect()
}

fn current(q: &Queue) -> i64 {
    q.current().unwrap().track.rowid
}

#[test]
fn play_next_and_enqueue() {
    let mut q = Queue::new();
    q.set(entries(&[1, 2, 3]), 1);
    assert_eq!(current(&q), 2);
    q.play_next(entries(&[10, 11]));
    assert_eq!(
        ids(&q),
        [1, 2, 10, 11, 3],
        "play next inserts after the current item"
    );
    q.enqueue(entries(&[20]));
    assert_eq!(ids(&q), [1, 2, 10, 11, 3, 20], "add to queue appends");
    assert_eq!(current(&q), 2);
    assert_eq!(q.advance(false).unwrap().track.rowid, 10);
}

#[test]
fn move_and_remove_keep_current() {
    let mut q = Queue::new();
    q.set(entries(&[1, 2, 3, 4, 5]), 2); // current = 3
    q.move_item(0, 4);
    assert_eq!(ids(&q), [2, 3, 4, 5, 1]);
    assert_eq!(current(&q), 3);
    q.move_item(4, 0);
    assert_eq!(ids(&q), [1, 2, 3, 4, 5]);
    assert_eq!(current(&q), 3);
    q.move_item(2, 4); // move the current item itself
    assert_eq!(ids(&q), [1, 2, 4, 5, 3]);
    assert_eq!(current(&q), 3);
    assert_eq!(q.cursor(), Some(4));

    q.remove(0);
    assert_eq!(ids(&q), [2, 4, 5, 3]);
    assert_eq!(current(&q), 3);
    q.remove(3); // remove current at the end → previous becomes current
    assert_eq!(ids(&q), [2, 4, 5]);
    assert_eq!(current(&q), 5);
    q.clear();
    assert!(q.is_empty() && q.current().is_none());
}

#[test]
fn shuffle_puts_current_first_and_unshuffle_restores() {
    let mut q = Queue::new();
    q.set(entries(&[1, 2, 3, 4, 5, 6]), 3); // current = 4
    let mut seq = [0usize, 1, 2, 0, 3].into_iter().cycle();
    let mut rng = move |n: usize| seq.next().unwrap() % n;
    q.set_shuffle_with(true, &mut rng);
    assert!(q.shuffle());
    assert_eq!(q.cursor(), Some(0));
    assert_eq!(current(&q), 4, "current item first");
    let mut shuffled = ids(&q);
    shuffled.sort_unstable();
    assert_eq!(shuffled, [1, 2, 3, 4, 5, 6], "a permutation");

    q.advance(false);
    let now = current(&q);
    q.set_shuffle(false);
    assert_eq!(ids(&q), [1, 2, 3, 4, 5, 6], "original order restored");
    assert_eq!(current(&q), now, "same item stays current");
}

#[test]
fn shuffled_insertions_survive_unshuffle() {
    let mut q = Queue::new();
    q.set(entries(&[1, 2, 3]), 0);
    q.set_shuffle(true);
    q.play_next(entries(&[9]));
    assert_eq!(
        q.get(1).unwrap().track.rowid,
        9,
        "plays next even when shuffled"
    );
    q.set_shuffle(false);
    assert_eq!(ids(&q), [1, 2, 3, 9]);
}

#[test]
fn repeat_modes() {
    let mut q = Queue::new();
    q.set(entries(&[1, 2]), 1);
    assert_eq!(q.peek_next(true), None, "repeat off ends");
    q.set_repeat(Repeat::All);
    assert_eq!(q.advance(true).unwrap().track.rowid, 1, "repeat all wraps");
    assert_eq!(q.peek_previous(), Some(1), "previous wraps too");
    q.set_repeat(Repeat::One);
    assert_eq!(
        q.advance(true).unwrap().track.rowid,
        1,
        "repeat one replays when the track ends"
    );
    assert_eq!(
        q.advance(false).unwrap().track.rowid,
        2,
        "manual next still moves on"
    );
    assert_eq!(Repeat::Off.cycle().cycle().cycle(), Repeat::Off);
}

#[test]
fn persistence_round_trip() {
    let db = Db::open_in_memory().unwrap();
    let album = Album {
        id: "al".into(),
        name: "A".into(),
        song: (1..=5)
            .map(|i| Song {
                id: format!("s{i}"),
                title: format!("S{i}"),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let rowid = db
        .with(|c| library::upsert_album_with_tracks(c, &album))
        .unwrap();
    let tracks = db
        .with(|c| lio_store::queries::album_track_rows(c, rowid))
        .unwrap();

    let mut q = Queue::new();
    let origin = Origin {
        kind: Some("album".into()),
        id: Some("al".into()),
    };
    q.set(
        tracks
            .iter()
            .cloned()
            .map(|t| QueueEntry::new(t, origin.clone()))
            .collect(),
        2,
    );
    q.set_shuffle(true);
    q.set_repeat(Repeat::All);
    q.position_ms = 42_000;
    db.with(|c| q.save(c)).unwrap();

    let restored = db.with(|c| Queue::load(c)).unwrap();
    assert_eq!(ids(&restored), ids(&q), "shuffle_order survives a reload");
    assert_eq!(current(&restored), current(&q));
    assert!(restored.shuffle());
    assert_eq!(restored.repeat(), Repeat::All);
    assert_eq!(
        restored.position_ms, 42_000,
        "restore resumes at position_ms (paused)"
    );
    assert_eq!(restored.current().unwrap().origin, origin);

    // A track deleted on the server disappears from the restored queue.
    db.with(|c| {
        Ok(c.execute("DELETE FROM track WHERE id = 's1'", [])
            .map(drop)?)
    })
    .unwrap();
    let restored = db.with(|c| Queue::load(c)).unwrap();
    assert_eq!(restored.len(), 4);
    assert!(restored.current().is_some());
}
