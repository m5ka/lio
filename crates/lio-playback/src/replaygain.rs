//! ReplayGain filter: `rgvolume ! rglimiter` as playbin's `audio-filter`.

use gst::prelude::*;

/// How gain is applied. `Off` removes the filter (applied on the next track start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplayGain {
    Off,
    #[default]
    Track,
    Album,
}

/// A bin wrapping `rgvolume ! rglimiter` with ghost pads.
#[derive(Debug, Clone)]
pub struct ReplayGainFilter {
    bin: gst::Bin,
    rgvolume: gst::Element,
}

impl ReplayGainFilter {
    pub fn new() -> Result<Self, gst::glib::BoolError> {
        let bin = gst::Bin::with_name("replaygain");
        let rgvolume = gst::ElementFactory::make("rgvolume")
            .property("pre-amp", 0.0f64)
            .build()?;
        let rglimiter = gst::ElementFactory::make("rglimiter").build()?;
        let convert = gst::ElementFactory::make("audioconvert").build()?;
        bin.add_many([&rgvolume, &rglimiter, &convert])?;
        gst::Element::link_many([&rgvolume, &rglimiter, &convert])?;
        let sink = gst::GhostPad::with_target(&rgvolume.static_pad("sink").unwrap())?;
        let src = gst::GhostPad::with_target(&convert.static_pad("src").unwrap())?;
        bin.add_pad(&sink)?;
        bin.add_pad(&src)?;
        Ok(Self { bin, rgvolume })
    }

    pub fn element(&self) -> &gst::Element {
        self.bin.upcast_ref()
    }

    pub fn set_album_mode(&self, album: bool) {
        self.rgvolume.set_property("album-mode", album);
    }

    /// Gain used when the stream has no ReplayGain tags, e.g. from the server's
    /// OpenSubsonic `replayGain` fields. `None` resets it to 0 dB.
    pub fn set_fallback_gain(&self, gain_db: Option<f64>) {
        self.rgvolume
            .set_property("fallback-gain", gain_db.unwrap_or(0.0).clamp(-60.0, 60.0));
    }
}
