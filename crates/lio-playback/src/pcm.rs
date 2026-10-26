//! Raw audio for visualisers that work from samples (projectM): a branch off the audio
//! sink that copies 32-bit float stereo samples, handed out once their audio is being
//! heard.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gst::prelude::*;

/// Samples are interleaved stereo.
pub const CHANNELS: usize = 2;
/// Samples handed out at most per call (per channel): enough for one display frame
/// even after a stall, without flooding the consumer.
const MAX_FRAMES: usize = 4096;
/// Samples waiting longer than this are left over from before a seek.
const STALE_NS: u64 = 2_000_000_000;
/// Buffers kept at most if nobody reads them.
const QUEUE_MAX: usize = 200;

#[derive(Default)]
struct Buffers {
    /// Samples by the running time their audio starts at, oldest first.
    queue: VecDeque<(u64, Vec<f32>)>,
}

pub(crate) struct Pcm {
    queue: gst::Element,
    convert: gst::Element,
    caps: gst::Element,
    sink: gst::Element,
    enabled: Arc<AtomicBool>,
    buffers: Arc<Mutex<Buffers>>,
}

impl Pcm {
    pub fn new() -> Result<Self, gst::glib::BoolError> {
        // Leaky and never waited on: this branch can't hold up playback.
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream")
            .property("max-size-time", 500_000_000u64)
            .build()?;
        let convert = gst::ElementFactory::make("audioconvert").build()?;
        let caps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("channels", CHANNELS as i32)
                    .field("layout", "interleaved")
                    .build(),
            )
            .build()?;
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build()?;
        let enabled = Arc::new(AtomicBool::new(false));
        let buffers = Arc::new(Mutex::new(Buffers::default()));
        let pad = sink.static_pad("sink").expect("fakesink has a sink pad");
        let (on, store) = (enabled.clone(), buffers.clone());
        pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            if !on.load(Ordering::Relaxed) {
                return gst::PadProbeReturn::Ok;
            }
            let Some(buffer) = info.buffer() else {
                return gst::PadProbeReturn::Ok;
            };
            let start = buffer.pts().and_then(|pts| {
                let segment = pad.sticky_event::<gst::event::Segment>(0)?;
                segment
                    .segment()
                    .downcast_ref::<gst::ClockTime>()?
                    .to_running_time(pts)
            });
            if let (Some(start), Ok(map)) = (start, buffer.map_readable()) {
                let samples = map
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect();
                let mut buffers = store.lock().unwrap();
                if buffers.queue.len() >= QUEUE_MAX {
                    buffers.queue.pop_front();
                }
                buffers.queue.push_back((start.nseconds(), samples));
            }
            gst::PadProbeReturn::Ok
        });
        Ok(Self {
            queue,
            convert,
            caps,
            sink,
            enabled,
            buffers,
        })
    }

    /// The branch's elements, in order, for adding to the audio sink bin.
    pub fn elements(&self) -> [&gst::Element; 4] {
        [&self.queue, &self.convert, &self.caps, &self.sink]
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        if !enabled {
            self.clear();
        }
    }

    /// Forgets queued samples (seek, new track, stop).
    pub fn clear(&self) {
        self.buffers.lock().unwrap().queue.clear();
    }

    /// Interleaved stereo samples whose audio has started playing by `running_time` and
    /// that haven't been handed out yet, at most `MAX_FRAMES` per channel (the latest).
    pub fn take(&self, running_time: Option<gst::ClockTime>) -> Vec<f32> {
        let Some(now) = running_time.map(gst::ClockTime::nseconds) else {
            return Vec::new();
        };
        let mut buffers = self.buffers.lock().unwrap();
        if buffers
            .queue
            .front()
            .is_some_and(|(start, _)| *start > now + STALE_NS)
        {
            buffers.queue.clear();
        }
        let mut out = Vec::new();
        while buffers
            .queue
            .front()
            .is_some_and(|(start, _)| *start <= now)
        {
            let (_, samples) = buffers.queue.pop_front().unwrap();
            out.extend(samples);
        }
        let keep = MAX_FRAMES * CHANNELS;
        if out.len() > keep {
            out.drain(..out.len() - keep);
        }
        out
    }
}
