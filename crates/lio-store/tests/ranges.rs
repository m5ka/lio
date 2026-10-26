//! Range bitmap: insert/merge/contains/first-missing and sidecar persistence.

use lio_store::ranges::Ranges;

#[test]
fn insert_merges_overlapping_and_adjacent() {
    let mut r = Ranges::new();
    r.insert(10, 20);
    r.insert(30, 40);
    assert_eq!(r.as_slice(), &[(10, 20), (30, 40)]);
    r.insert(20, 25); // adjacent to the first
    assert_eq!(r.as_slice(), &[(10, 25), (30, 40)]);
    r.insert(24, 31); // bridges both
    assert_eq!(r.as_slice(), &[(10, 40)]);
    r.insert(0, 5);
    r.insert(50, 60);
    r.insert(2, 55); // swallows everything
    assert_eq!(r.as_slice(), &[(0, 60)]);
    r.insert(7, 7); // empty is ignored
    assert_eq!(r.as_slice(), &[(0, 60)]);
}

#[test]
fn out_of_order_inserts() {
    let mut r = Ranges::new();
    for (s, e) in [(90, 100), (0, 10), (40, 50), (10, 20), (60, 70)] {
        r.insert(s, e);
    }
    assert_eq!(r.as_slice(), &[(0, 20), (40, 50), (60, 70), (90, 100)]);
    assert_eq!(r.total_bytes(), 50);
}

#[test]
fn contains_and_available() {
    let mut r = Ranges::new();
    r.insert(0, 100);
    r.insert(200, 300);
    assert!(r.contains(0, 100));
    assert!(r.contains(50, 60));
    assert!(!r.contains(50, 150));
    assert!(!r.contains(100, 101));
    assert!(r.contains(250, 300));
    assert!(r.contains(5, 5));
    assert_eq!(r.available_from(0), 100);
    assert_eq!(r.available_from(90), 10);
    assert_eq!(r.available_from(100), 0);
    assert_eq!(r.available_from(150), 0);
    assert_eq!(r.available_from(299), 1);
}

#[test]
fn first_missing() {
    let mut r = Ranges::new();
    assert_eq!(r.first_missing(0, 1000), Some((0, 1000)));
    r.insert(0, 100);
    r.insert(200, 300);
    assert_eq!(r.first_missing(0, 1000), Some((100, 200)));
    assert_eq!(r.first_missing(150, 1000), Some((150, 200)));
    assert_eq!(r.first_missing(250, 1000), Some((300, 1000)));
    assert_eq!(r.first_missing(250, 300), None);
    assert_eq!(r.next_missing(250, 300), Some((100, 200)));
    r.insert(100, 1000);
    assert_eq!(r.first_missing(0, 1000), None);
    assert!(r.covers(1000));
    assert!(!r.covers(1001));
}

#[test]
fn sidecar_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("42.original.part");
    let sidecar = Ranges::sidecar_path(&file);
    assert!(
        sidecar
            .to_string_lossy()
            .ends_with("42.original.part.ranges")
    );

    let mut r = Ranges::new();
    r.insert(0, 4096);
    r.insert(8192, 10_000);
    r.save(&sidecar).unwrap();
    assert_eq!(Ranges::load(&sidecar), r);

    std::fs::write(&sidecar, "garbage\n").unwrap();
    assert!(Ranges::load(&sidecar).is_empty());
    assert!(Ranges::load(&dir.path().join("missing.ranges")).is_empty());
}
