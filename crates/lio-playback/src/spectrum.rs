//! Spectrum analysis for visualisers: a `spectrum` element in front of the audio sink,
//! whose results are held back until the audio they describe is being heard.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use gst::prelude::*;

/// FFT bands over 0 Hz to the Nyquist frequency.
pub const BANDS: u32 = 1024;
/// Magnitudes are reported down to this many dB; anything quieter reads as this.
pub const FLOOR_DB: f32 = -90.0;
/// One result per display frame at 60 Hz.
const INTERVAL_NS: u64 = 1_000_000_000 / 60;
/// Results waiting longer than this are left over from before a seek.
const STALE_NS: u64 = 2_000_000_000;
/// Results waiting at most; older ones are dropped if nobody reads them.
const QUEUE_MAX: usize = 120;

/// The spectrum of what is playing right now.
#[derive(Debug, Clone, PartialEq)]
pub struct SpectrumFrame {
    /// Magnitude per band in dB, at least [`FLOOR_DB`]. Band `i` is centred on
    /// `(i + 0.5) * nyquist / bands` Hz.
    pub db: Vec<f32>,
    /// Half the sample rate: the top of the last band, in Hz.
    pub nyquist: f32,
}

#[derive(Default)]
struct Frames {
    /// Results by running time at which their audio ends, oldest first.
    queue: VecDeque<(u64, SpectrumFrame)>,
    current: Option<SpectrumFrame>,
}

pub(crate) struct Spectrum {
    element: gst::Element,
    frames: Arc<Mutex<Frames>>,
}

impl Spectrum {
    pub fn new() -> Result<Self, gst::glib::BoolError> {
        let element = gst::ElementFactory::make("spectrum")
            .name("lio-spectrum")
            .property("bands", BANDS)
            .property("threshold", FLOOR_DB as i32)
            .property("interval", INTERVAL_NS)
            .property("post-messages", false)
            .property("message-magnitude", true)
            .property("multi-channel", false)
            .build()?;
        Ok(Self {
            element,
            frames: Arc::default(),
        })
    }

    /// The `spectrum` element, for the audio sink bin.
    pub fn element(&self) -> &gst::Element {
        &self.element
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.element.set_property("post-messages", enabled);
        if !enabled {
            self.clear();
        }
    }

    /// Forgets queued results (seek, new track, stop).
    pub fn clear(&self) {
        let mut frames = self.frames.lock().unwrap();
        frames.queue.clear();
        frames.current = None;
    }

    /// Takes a spectrum result off the bus. Returns `false` for other messages. Runs on
    /// the streaming thread (bus sync handler).
    pub fn handle_message(&self, msg: &gst::Message) -> bool {
        let gst::MessageView::Element(element) = msg.view() else {
            return false;
        };
        if msg.src() != Some(self.element.upcast_ref()) {
            return false;
        }
        let Some(s) = element.structure().filter(|s| s.name() == "spectrum") else {
            return false;
        };
        // `endtime` is in stream time, which differs from running time after a seek.
        let (Ok(start), Ok(duration), Ok(magnitudes)) = (
            s.get::<u64>("running-time"),
            s.get::<u64>("duration"),
            s.get::<gst::List>("magnitude"),
        ) else {
            return true;
        };
        let end = start + duration;
        let db = magnitudes
            .iter()
            .map(|v| v.get::<f32>().map_or(FLOOR_DB, |db| db.max(FLOOR_DB)))
            .collect();
        let nyquist = self
            .element
            .static_pad("sink")
            .and_then(|pad| pad.current_caps())
            .and_then(|caps| gst_audio::AudioInfo::from_caps(&caps).ok())
            .map_or(22_050.0, |info| info.rate() as f32 / 2.0);
        let mut frames = self.frames.lock().unwrap();
        if frames.queue.len() >= QUEUE_MAX {
            frames.queue.pop_front();
        }
        frames.queue.push_back((end, SpectrumFrame { db, nyquist }));
        true
    }

    /// The latest result whose audio has started playing by `running_time`.
    pub fn current(&self, running_time: Option<gst::ClockTime>) -> Option<SpectrumFrame> {
        let mut frames = self.frames.lock().unwrap();
        if let Some(now) = running_time.map(gst::ClockTime::nseconds) {
            // Results describe the interval before their end time.
            let start = |end: u64| end.saturating_sub(INTERVAL_NS);
            if frames
                .queue
                .front()
                .is_some_and(|(end, _)| start(*end) > now + STALE_NS)
            {
                frames.queue.clear();
            }
            while frames
                .queue
                .front()
                .is_some_and(|(end, _)| start(*end) <= now)
            {
                let (_, frame) = frames.queue.pop_front().unwrap();
                frames.current = Some(frame);
            }
        }
        frames.current.clone()
    }
}
