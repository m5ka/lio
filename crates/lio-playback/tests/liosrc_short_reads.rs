//! `liosrc` with a source whose reads come back short (bytes still arriving): every
//! fixture format must play to the end. Ogg demuxing passes its own buffers in pull mode,
//! which used to panic on a short read.
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gst::prelude::*;
use lio_playback::{TrackResolver, TrackSource};

mod common;

struct Trickle {
    data: Arc<Vec<u8>>,
    start: Instant,
    cancelled: AtomicBool,
}

impl Trickle {
    fn available(&self) -> usize {
        // 1 KB at once, then 64 KB/s.
        (1024 + self.start.elapsed().as_millis() as usize * 64).min(self.data.len())
    }
}

impl TrackSource for Trickle {
    fn size(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let offset = offset as usize;
        if offset >= self.data.len() {
            return Ok(0);
        }
        // Like TrackHandle: far reads wait, near reads return what's there.
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let avail = self.available();
            if avail > offset {
                let end = avail.min(offset + buf.len());
                buf[..end - offset].copy_from_slice(&self.data[offset..end]);
                return Ok(end - offset);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    fn resume(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }
}

struct Files(Mutex<Vec<(String, Arc<Vec<u8>>)>>);

impl TrackResolver for Files {
    fn open(&self, id: &str, _q: &str) -> Result<Box<dyn TrackSource>, String> {
        let data = self
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| n == id)
            .unwrap()
            .1
            .clone();
        Ok(Box::new(Trickle {
            data,
            start: Instant::now(),
            cancelled: AtomicBool::new(false),
        }))
    }
}

#[test]
fn plays_every_format_with_short_reads() {
    lio_playback::init().unwrap();
    let Some(dir) = common::fixture_album("Format Mix") else {
        return;
    };
    let paths: Vec<String> = ["01 - FLAC.flac", "02 - Opus.opus", "03 - MP3.mp3"]
        .iter()
        .map(|f| dir.join(f).to_string_lossy().into_owned())
        .collect();
    let files: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| (format!("f{i}"), Arc::new(std::fs::read(p).unwrap())))
        .collect();
    lio_playback::set_resolver(Arc::new(Files(Mutex::new(files))));
    let mut failed = vec![];
    for (i, p) in paths.iter().enumerate() {
        let pb = gst::ElementFactory::make("playbin3")
            .property("uri", lio_playback::track_uri(&format!("f{i}"), "original"))
            .property(
                "audio-sink",
                gst::ElementFactory::make("fakesink")
                    .property("sync", false)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();
        pb.set_state(gst::State::Playing).unwrap();
        let bus = pb.bus().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut result = "timeout".to_string();
        while Instant::now() < deadline {
            if let Some(m) = bus.timed_pop(gst::ClockTime::from_mseconds(50)) {
                match m.view() {
                    gst::MessageView::Error(e) => {
                        result = format!("ERROR {} {:?}", e.error(), e.debug());
                        break;
                    }
                    gst::MessageView::Eos(_) => {
                        result = "ok".into();
                        break;
                    }
                    _ => {}
                }
            }
        }
        pb.set_state(gst::State::Null).unwrap();
        eprintln!("{p}: {result}");
        if result != "ok" {
            failed.push(p.clone());
        }
    }
    assert!(failed.is_empty(), "failed: {failed:?}");
}
