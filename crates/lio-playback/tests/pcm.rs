//! Samples for sample-based visualisers: delivered as they are heard, at the real rate,
//! with the music's actual waveform.

use std::time::{Duration, Instant};

use gst::glib;
use gst::prelude::*;
use lio_playback::{PCM_CHANNELS, Player};

mod common;

#[test]
fn samples_arrive_as_they_are_heard() {
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
    player.play_uri(&uri, None, false).unwrap();
    let _ = player.playbin().state(gst::ClockTime::from_seconds(2));
    std::thread::sleep(Duration::from_millis(300));
    assert!(player.pcm().is_empty(), "nothing while copying is off");

    player.set_pcm_enabled(true);
    let _ = player.pcm();
    let (start, mut samples) = (Instant::now(), Vec::new());
    while start.elapsed() < Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(16));
        samples.extend(player.pcm());
    }
    let seconds = start.elapsed().as_secs_f32();
    let frames = samples.len() / PCM_CHANNELS;
    // About real time: 44.1 kHz, give or take a buffer at either end.
    let rate = frames as f32 / seconds;
    assert!((30_000.0..55_000.0).contains(&rate), "{rate} frames/s");
    // The sweep is a 0.4-amplitude sine near 200-220 Hz here.
    let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!((0.3..0.5).contains(&peak), "peak {peak}");
    let left: Vec<f32> = samples.iter().step_by(PCM_CHANNELS).copied().collect();
    let crossings = left
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    let hz = crossings as f32 / (left.len() as f32 / 44_100.0);
    assert!((150.0..300.0).contains(&hz), "{hz} Hz");

    player.set_pcm_enabled(false);
    std::thread::sleep(Duration::from_millis(100));
    assert!(player.pcm().is_empty(), "nothing once copying is off again");
}
