//! Gapless playback: five tracks cut sample-exactly from one sweep must play
//! with no running-time gap at any boundary.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gst::prelude::*;
use lio_playback::{Player, PlayerEvent};

mod common;

fn fixture_tracks() -> Option<Vec<String>> {
    let dir = common::fixture_album("Gapless Sweep")?;
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "flac"))
        .collect();
    files.sort();
    let uris = files
        .iter()
        .map(|p| {
            gst::glib::filename_to_uri(p.canonicalize().unwrap(), None)
                .unwrap()
                .into()
        })
        .collect();
    Some(uris)
}

#[derive(Default)]
struct Timeline {
    segment: Option<gst::FormattedSegment<gst::ClockTime>>,
    last_end: Option<gst::ClockTime>,
    max_gap: gst::ClockTime,
    total: gst::ClockTime,
    segments: u32,
}

#[test]
fn five_tracks_play_without_gaps() {
    lio_playback::init().unwrap();
    let Some(tracks) = fixture_tracks() else {
        return;
    };
    assert_eq!(tracks.len(), 5);

    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .unwrap();
    let timeline = Arc::new(Mutex::new(Timeline::default()));
    {
        let timeline = timeline.clone();
        sink.static_pad("sink").unwrap().add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_, info| {
                let mut t = timeline.lock().unwrap();
                match &info.data {
                    Some(gst::PadProbeData::Event(ev)) => {
                        if let gst::EventView::Segment(s) = ev.view() {
                            t.segment = s.segment().clone().downcast::<gst::ClockTime>().ok();
                            t.segments += 1;
                        }
                    }
                    Some(gst::PadProbeData::Buffer(buf)) => {
                        let (Some(seg), Some(pts), Some(dur)) =
                            (t.segment.clone(), buf.pts(), buf.duration())
                        else {
                            return gst::PadProbeReturn::Ok;
                        };
                        let start = seg.to_running_time(pts).unwrap();
                        let end = seg.to_running_time(pts + dur).unwrap();
                        if let Some(last) = t.last_end {
                            let gap = if start > last {
                                start - last
                            } else {
                                last - start
                            };
                            t.max_gap = t.max_gap.max(gap);
                        }
                        t.last_end = Some(end);
                        t.total += dur;
                    }
                    _ => {}
                }
                gst::PadProbeReturn::Ok
            },
        );
    }

    let player = Player::new(Some(sink)).unwrap();
    let list = tracks.clone();
    player.set_next_provider(Some(Arc::new(move |current: Option<&str>| {
        let i = list.iter().position(|u| Some(u.as_str()) == current)?;
        list.get(i + 1).cloned()
    })));
    player.play_uri(&tracks[0], None, false).unwrap();

    let bus = player.bus();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut started = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "playback did not finish in time");
        let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(200)) else {
            continue;
        };
        match player.translate_message(&msg) {
            Some(PlayerEvent::StreamStarted { uri }) => started.push(uri),
            Some(PlayerEvent::Eos) => break,
            Some(PlayerEvent::Error { message, .. }) => panic!("playback error: {message}"),
            _ => {}
        }
    }

    assert_eq!(started, tracks, "every track started once, in order");
    let t = timeline.lock().unwrap();
    assert!(
        t.max_gap <= gst::ClockTime::from_mseconds(1),
        "gap of {} at a track boundary",
        t.max_gap
    );
    let total_ms = t.total.mseconds();
    assert!(
        (99_900..=100_100).contains(&total_ms),
        "played {total_ms} ms, expected 100 s"
    );
}
