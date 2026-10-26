//! `liosrc` against an in-memory `TrackSource`: seeking, blocking reads, cancellation, EOS.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use gst::prelude::*;
use lio_playback::{TrackResolver, TrackSource};

mod common;

/// Serves `data`, but only the first `available` bytes until more are released.
struct MemSource {
    data: Arc<Vec<u8>>,
    gate: Arc<(Mutex<usize>, Condvar)>,
    cancelled: AtomicBool,
}

impl TrackSource for MemSource {
    fn size(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let offset = offset as usize;
        if offset >= self.data.len() {
            return Ok(0);
        }
        let (lock, cvar) = &*self.gate;
        let mut available = lock.lock().unwrap();
        while *available <= offset {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            available = cvar
                .wait_timeout(available, Duration::from_millis(20))
                .unwrap()
                .0;
        }
        let end = (*available).min(offset + buf.len()).min(self.data.len());
        buf[..end - offset].copy_from_slice(&self.data[offset..end]);
        Ok(end - offset)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.gate.1.notify_all();
    }

    fn resume(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }
}

struct MemResolver {
    data: Arc<Vec<u8>>,
    gate: Arc<(Mutex<usize>, Condvar)>,
}

impl TrackResolver for MemResolver {
    fn open(&self, track_id: &str, _quality: &str) -> Result<Box<dyn TrackSource>, String> {
        if track_id == "missing" {
            return Err("no such track".into());
        }
        Ok(Box::new(MemSource {
            data: self.data.clone(),
            gate: self.gate.clone(),
            cancelled: AtomicBool::new(false),
        }))
    }
}

fn fixture_bytes() -> Option<Vec<u8>> {
    let path = common::fixture_album("Gapless Sweep")?.join("01 - Sweep Part 1.flac");
    Some(std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path:?}")))
}

fn pipeline(uri: &str) -> (gst::Pipeline, gst::Element) {
    let pipeline = gst::Pipeline::new();
    let src = gst::Element::make_from_uri(gst::URIType::Src, uri, None).unwrap();
    let decode = gst::ElementFactory::make("decodebin").build().unwrap();
    let convert = gst::ElementFactory::make("audioconvert").build().unwrap();
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .unwrap();
    pipeline.add_many([&src, &decode, &convert, &sink]).unwrap();
    src.link(&decode).unwrap();
    convert.link(&sink).unwrap();
    let convert_weak = convert.downgrade();
    decode.connect_pad_added(move |_, pad| {
        if let Some(convert) = convert_weak.upgrade() {
            let _ = pad.link(&convert.static_pad("sink").unwrap());
        }
    });
    (pipeline, src)
}

fn wait_for(bus: &gst::Bus, timeout: Duration, f: impl Fn(&gst::Message) -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(50)) {
            if let gst::MessageView::Error(e) = msg.view() {
                panic!("pipeline error: {} {:?}", e.error(), e.debug());
            }
            if f(&msg) {
                return true;
            }
        }
    }
    false
}

/// All of this shares one global resolver, so it runs as a single test.
#[test]
fn liosrc_reads_seeks_blocks_and_cancels() {
    lio_playback::init().unwrap();
    let Some(data) = fixture_bytes() else {
        return;
    };
    let data = Arc::new(data);
    let gate = Arc::new((Mutex::new(data.len()), Condvar::new()));
    lio_playback::set_resolver(Arc::new(MemResolver {
        data: data.clone(),
        gate: gate.clone(),
    }));

    // 1. URI handler picks liosrc, plays to EOS.
    let uri = lio_playback::track_uri("t1", "original");
    let (pipe, src) = pipeline(&uri);
    assert_eq!(src.factory().unwrap().name(), "liosrc");
    pipe.set_state(gst::State::Playing).unwrap();
    let bus = pipe.bus().unwrap();
    assert!(wait_for(&bus, Duration::from_secs(10), |m| matches!(
        m.view(),
        gst::MessageView::Eos(_)
    )));
    pipe.set_state(gst::State::Null).unwrap();

    // 2. Seeking to arbitrary positions works and playback continues to EOS.
    let (pipe, _) = pipeline(&uri);
    pipe.set_state(gst::State::Paused).unwrap();
    let bus = pipe.bus().unwrap();
    assert!(wait_for(&bus, Duration::from_secs(10), |m| matches!(
        m.view(),
        gst::MessageView::AsyncDone(_)
    )));
    for secs in [15u64, 3, 11] {
        pipe.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_seconds(secs),
        )
        .unwrap_or_else(|_| panic!("seek to {secs}s failed"));
        assert!(wait_for(&bus, Duration::from_secs(10), |m| matches!(
            m.view(),
            gst::MessageView::AsyncDone(_)
        )));
        let pos = pipe.query_position::<gst::ClockTime>().unwrap();
        assert!(
            pos.seconds().abs_diff(secs) <= 1,
            "position {pos} after seeking to {secs}s"
        );
    }
    pipe.set_state(gst::State::Playing).unwrap();
    assert!(wait_for(&bus, Duration::from_secs(10), |m| matches!(
        m.view(),
        gst::MessageView::Eos(_)
    )));
    pipe.set_state(gst::State::Null).unwrap();

    // 3. A read blocked on missing bytes is released by a state change (cancel → EOS-free stop).
    *gate.0.lock().unwrap() = data.len() / 3;
    let (pipe, _) = pipeline(&uri);
    pipe.set_state(gst::State::Playing).unwrap();
    let bus = pipe.bus().unwrap();
    assert!(
        !wait_for(&bus, Duration::from_millis(500), |m| matches!(
            m.view(),
            gst::MessageView::Eos(_)
        )),
        "must not reach EOS while bytes are missing"
    );
    let started = Instant::now();
    pipe.set_state(gst::State::Null).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "stop must unblock the pending read"
    );

    // 4. Releasing the bytes lets a blocked stream finish.
    let (pipe, _) = pipeline(&uri);
    pipe.set_state(gst::State::Playing).unwrap();
    let bus = pipe.bus().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    {
        let (lock, cvar) = &*gate;
        *lock.lock().unwrap() = data.len();
        cvar.notify_all();
    }
    assert!(wait_for(&bus, Duration::from_secs(10), |m| matches!(
        m.view(),
        gst::MessageView::Eos(_)
    )));
    pipe.set_state(gst::State::Null).unwrap();

    // 5. Resolver errors surface as an element error, not a hang.
    let (pipe, _) = pipeline(&lio_playback::track_uri("missing", "original"));
    assert!(
        pipe.set_state(gst::State::Playing).is_err() || {
            let bus = pipe.bus().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut errored = false;
            while Instant::now() < deadline && !errored {
                if let Some(m) = bus.timed_pop(gst::ClockTime::from_mseconds(50)) {
                    errored = matches!(m.view(), gst::MessageView::Error(_));
                }
            }
            errored
        }
    );
    pipe.set_state(gst::State::Null).unwrap();
}
