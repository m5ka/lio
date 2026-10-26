//! The spectrum follows what is being heard: on the gapless sweep (a chirp rising from
//! 200 Hz at 18 Hz per second) the loudest band tracks the playback position, also after
//! a seek.

use std::time::{Duration, Instant};

use gst::glib;
use gst::prelude::*;
use lio_playback::Player;

mod common;

/// The sweep's frequency at `ms` into part 1.
fn sweep_hz(ms: u64) -> f32 {
    200.0 + 18.0 * ms as f32 / 1000.0
}

fn peak_hz(player: &Player) -> Option<(f32, f32)> {
    let frame = player.spectrum()?;
    let band_hz = frame.nyquist / frame.db.len() as f32;
    let (i, level) = frame
        .db
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))?;
    Some(((i as f32 + 0.5) * band_hz, *level))
}

/// Checks the loudest band against the sweep for a while, allowing a band either way.
fn assert_tracks_sweep(player: &Player) {
    let end = Instant::now() + Duration::from_millis(800);
    let mut checked = 0;
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
        let (Some((hz, level)), Some(ms)) = (peak_hz(player), player.position_ms()) else {
            continue;
        };
        let expected = sweep_hz(ms);
        assert!(level > -30.0, "peak at {level} dB");
        assert!(
            (hz - expected).abs() < 150.0,
            "loudest band at {hz} Hz, sweep at {expected} Hz ({ms} ms)"
        );
        checked += 1;
    }
    assert!(checked >= 5, "only {checked} spectrum frames");
}

#[test]
fn spectrum_follows_playback() {
    lio_playback::init().unwrap();
    let Some(dir) = common::fixture_album("Gapless Sweep") else {
        return;
    };
    let path = dir.join("01 - Sweep Part 1.flac").canonicalize().unwrap();
    let uri: String = glib::filename_to_uri(path, None).unwrap().into();

    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", true)
        .build()
        .unwrap();
    let player = Player::new(Some(sink)).unwrap();
    assert!(player.spectrum().is_none(), "off until enabled");
    player.set_spectrum_enabled(true);
    player.play_uri(&uri, None, false).unwrap();
    let _ = player.playbin().state(gst::ClockTime::from_seconds(2));
    std::thread::sleep(Duration::from_millis(300));
    assert_tracks_sweep(&player);

    // After a seek, old results are dropped and new ones follow the new position.
    assert!(player.seek_ms(15_000));
    let _ = player.playbin().state(gst::ClockTime::from_seconds(2));
    std::thread::sleep(Duration::from_millis(300));
    assert_tracks_sweep(&player);

    player.set_spectrum_enabled(false);
    assert!(player.spectrum().is_none(), "cleared when disabled");
}
