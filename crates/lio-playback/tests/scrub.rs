//! Scrubbing: a burst of seeks (dragging the seek bar back and forth, holding a seek
//! key) must leave the player playing from the last target, for local files and for
//! `liosrc` streams whose reads take a while.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gst::glib;
use gst::prelude::*;
use lio_playback::{Player, TrackResolver, TrackSource};

mod common;

fn fixture_path() -> Option<std::path::PathBuf> {
    let dir = common::fixture_album("Gapless Sweep")?;
    Some(dir.join("01 - Sweep Part 1.flac").canonicalize().unwrap())
}

/// Serves `data` with a delay per read, like a stream arriving over the network.
struct SlowSource {
    data: Arc<Vec<u8>>,
    cancelled: AtomicBool,
}

impl TrackSource for SlowSource {
    fn size(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        std::thread::sleep(Duration::from_millis(15));
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let offset = (offset as usize).min(self.data.len());
        let end = (offset + buf.len()).min(self.data.len());
        buf[..end - offset].copy_from_slice(&self.data[offset..end]);
        Ok(end - offset)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn resume(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }
}

struct SlowResolver(Arc<Vec<u8>>);

impl TrackResolver for SlowResolver {
    fn open(&self, _track_id: &str, _quality: &str) -> Result<Box<dyn TrackSource>, String> {
        Ok(Box::new(SlowSource {
            data: self.0.clone(),
            cancelled: AtomicBool::new(false),
        }))
    }
}

fn run_for(ctx: &glib::MainContext, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn scrub(uri: &str) {
    let ctx = glib::MainContext::new();
    ctx.with_thread_default(|| {
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", true)
            .build()
            .unwrap();
        let player = Player::new(Some(sink)).unwrap();
        let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
        player.connect_events({
            let errors = errors.clone();
            move |event| {
                if let lio_playback::PlayerEvent::Error { message, .. } = event {
                    errors.lock().unwrap().push(message);
                }
            }
        });
        player.play_uri(uri, None, false).unwrap();
        let (_, state, _) = player.playbin().state(gst::ClockTime::from_seconds(5));
        assert_eq!(state, gst::State::Playing);
        run_for(&ctx, Duration::from_millis(300));

        // Drag back and forth: a seek every few ms, never waiting for one to finish.
        let mut target = 0;
        for i in 0..200u64 {
            target = (i * 700) % 15_000;
            player.seek_ms(target);
            run_for(&ctx, Duration::from_millis(4));
        }
        let end = Instant::now() + Duration::from_secs(5);
        while player.playbin().state(gst::ClockTime::ZERO).0 == Ok(gst::StateChangeSuccess::Async)
            && Instant::now() < end
        {
            run_for(&ctx, Duration::from_millis(20));
        }
        run_for(&ctx, Duration::from_millis(300));
        assert_eq!(*errors.lock().unwrap(), Vec::<String>::new());
        let (_, state, _) = player.playbin().state(gst::ClockTime::from_seconds(2));
        assert_eq!(
            state,
            gst::State::Playing,
            "stopped playing after scrubbing"
        );
        let before = player.position_ms().expect("position");
        assert!(
            before.abs_diff(target) < 2_000,
            "position {before} ms, last seek to {target} ms"
        );
        run_for(&ctx, Duration::from_millis(500));
        let after = player.position_ms().expect("position");
        assert!(
            after > before + 200,
            "not advancing: {before} -> {after} ms"
        );
    })
    .unwrap();
}

#[test]
fn scrubbing_a_file_keeps_playing() {
    lio_playback::init().unwrap();
    let Some(path) = fixture_path() else {
        return;
    };
    scrub(&glib::filename_to_uri(path, None).unwrap());
}

#[test]
fn scrubbing_a_slow_stream_keeps_playing() {
    lio_playback::init().unwrap();
    let Some(path) = fixture_path() else {
        return;
    };
    lio_playback::set_resolver(Arc::new(SlowResolver(Arc::new(
        std::fs::read(path).unwrap(),
    ))));
    scrub(&lio_playback::track_uri("t1", "original"));
}
