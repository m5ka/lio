//! GStreamer playback engine for Lio: playbin3 with gapless switching, ReplayGain,
//! and the `liosrc` element that reads from Lio's TrackStore.
//!
//! No GTK dependency.

pub mod liosrc;
mod pcm;
mod player;
mod replaygain;
mod source;
mod spectrum;

pub use pcm::CHANNELS as PCM_CHANNELS;
pub use player::{Error, NextProvider, Player, PlayerEvent, PlayerState};
pub use replaygain::{ReplayGain, ReplayGainFilter};
pub use source::{
    TrackResolver, TrackSource, parse_track_uri, set_resolver, track_uri, track_uri_keyed,
};
pub use spectrum::{BANDS as SPECTRUM_BANDS, FLOOR_DB as SPECTRUM_FLOOR_DB, SpectrumFrame};

/// Initialises GStreamer and registers `liosrc`. Safe to call more than once.
pub fn init() -> Result<(), gst::glib::Error> {
    gst::init()?;
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        if let Err(e) = liosrc::register_static() {
            tracing::error!("failed to register liosrc: {e}");
        }
    });
    Ok(())
}
