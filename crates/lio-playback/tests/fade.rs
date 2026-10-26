//! Pause/resume fades: the pipeline keeps playing while the volume ramps, pauses when the
//! ramp ends, and resuming mid-fade turns the fade around without pausing.

use std::time::{Duration, Instant};

use gst::glib;
use gst::prelude::*;
use lio_playback::Player;

mod common;

const FADE: Duration = Duration::from_millis(300);

fn first_track() -> Option<String> {
    let dir = common::fixture_album("Gapless Sweep")?;
    let path = dir.join("01 - Sweep Part 1.flac").canonicalize().unwrap();
    Some(glib::filename_to_uri(path, None).unwrap().into())
}

/// Runs `ctx` for `duration`, so fade timers fire.
fn run_for(ctx: &glib::MainContext, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn state(player: &Player) -> gst::State {
    player.playbin().state(gst::ClockTime::from_seconds(2)).1
}

fn volume(player: &Player) -> f64 {
    player.playbin().property("volume")
}

fn playing_player(uri: &str) -> Player {
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", true)
        .build()
        .unwrap();
    let player = Player::new(Some(sink)).unwrap();
    player.set_fade_duration(FADE);
    player.play_uri(uri, None, false).unwrap();
    assert_eq!(state(&player), gst::State::Playing);
    player
}

#[test]
fn pause_and_resume_fade() {
    lio_playback::init().unwrap();
    let Some(uri) = first_track() else {
        return;
    };
    let ctx = glib::MainContext::new();
    ctx.with_thread_default(|| {
        let player = playing_player(&uri);
        player.set_volume(0.5);
        let full = volume(&player);

        // Fade out: still playing and getting quieter, then paused and silent.
        player.pause();
        assert!(player.is_fading_out());
        run_for(&ctx, FADE / 2);
        assert_eq!(state(&player), gst::State::Playing);
        let halfway = volume(&player);
        assert!(halfway > 0.0 && halfway < full, "halfway volume {halfway}");
        // The user's volume isn't touched by the fade.
        assert!((player.volume() - 0.5).abs() < 1e-6);
        run_for(&ctx, FADE);
        assert!(!player.is_fading_out());
        assert_eq!(state(&player), gst::State::Paused);
        assert_eq!(volume(&player), 0.0);

        // Fade in: playing at once, back to the user's volume when done.
        player.play();
        assert_eq!(state(&player), gst::State::Playing);
        assert!(volume(&player) < full);
        run_for(&ctx, FADE + Duration::from_millis(100));
        assert!((volume(&player) - full).abs() < 1e-9);

        // Resuming mid fade-out turns it around without ever pausing.
        player.pause();
        run_for(&ctx, FADE / 3);
        player.play();
        assert!(!player.is_fading_out());
        run_for(&ctx, FADE + Duration::from_millis(100));
        assert_eq!(state(&player), gst::State::Playing);
        assert!((volume(&player) - full).abs() < 1e-9);
    })
    .unwrap();
}

#[test]
fn no_fade_pauses_at_once() {
    lio_playback::init().unwrap();
    let Some(uri) = first_track() else {
        return;
    };
    let ctx = glib::MainContext::new();
    ctx.with_thread_default(|| {
        let player = playing_player(&uri);
        player.set_fade_duration(Duration::ZERO);
        let full = volume(&player);

        player.pause();
        assert!(!player.is_fading_out());
        assert_eq!(state(&player), gst::State::Paused);
        player.play();
        assert_eq!(state(&player), gst::State::Playing);
        assert!((volume(&player) - full).abs() < 1e-9);
    })
    .unwrap();
}
