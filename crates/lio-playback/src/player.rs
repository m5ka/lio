//! `Player`: playbin3 wrapper with gapless track switching.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use gst::prelude::*;

use crate::replaygain::{ReplayGain, ReplayGainFilter};

/// Playback states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayerState {
    #[default]
    Idle,
    Loading,
    Playing,
    Paused,
    Buffering,
    Stopped,
    Error,
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    State(PlayerState),
    /// A new stream started rendering (first track, or a gapless switch).
    StreamStarted {
        uri: String,
    },
    /// The last track ended and no next URI was set.
    Eos,
    /// Playing `uri` failed (unplayable format, read timeout, ...).
    Error {
        uri: Option<String>,
        message: String,
    },
    DurationChanged,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("GStreamer element missing: {0}")]
    MissingElement(String),
    #[error("GStreamer: {0}")]
    Gst(String),
}

impl From<gst::glib::BoolError> for Error {
    fn from(e: gst::glib::BoolError) -> Self {
        Self::MissingElement(e.to_string())
    }
}

impl From<gst::StateChangeError> for Error {
    fn from(e: gst::StateChangeError) -> Self {
        Self::Gst(e.to_string())
    }
}

/// Called on `about-to-finish` with the current URI; returns the next one.
pub type NextProvider = Arc<dyn Fn(Option<&str>) -> Option<String> + Send + Sync>;

struct Shared {
    /// URI to switch to on `about-to-finish`.
    next_uri: Mutex<Option<String>>,
    /// Consulted when `next_uri` is empty.
    next_provider: Mutex<Option<NextProvider>>,
    /// Set when playback is requested; cleared by the first buffer reaching the sink.
    latency_start: Mutex<Option<Instant>>,
}

pub struct Player {
    playbin: gst::Element,
    shared: Arc<Shared>,
    replaygain: Option<ReplayGainFilter>,
    replaygain_mode: Mutex<ReplayGain>,
    _bus_watch: Mutex<Option<gst::bus::BusWatchGuard>>,
}

impl Player {
    /// Creates a player. `sink` overrides the audio sink; otherwise `LIO_AUDIO_SINK`
    /// (e.g. `fakesink`) or `autoaudiosink` is used.
    pub fn new(sink: Option<gst::Element>) -> Result<Self, Error> {
        let playbin = gst::ElementFactory::make("playbin3")
            .name("lio-playbin")
            .build()
            .map_err(|_| Error::MissingElement("playbin3".into()))?;

        // Audio only: no video or subtitles.
        let flags = playbin.property_value("flags");
        let flags_class = gst::glib::FlagsClass::with_type(flags.type_()).unwrap();
        let flags = flags_class
            .builder_with_value(flags)
            .unwrap()
            .unset_by_nick("video")
            .unset_by_nick("text")
            .build()
            .unwrap();
        playbin.set_property_from_value("flags", &flags);

        let sink = match sink {
            Some(sink) => sink,
            None => {
                let name =
                    std::env::var("LIO_AUDIO_SINK").unwrap_or_else(|_| "autoaudiosink".into());
                let sink = gst::ElementFactory::make(&name)
                    .build()
                    .map_err(|_| Error::MissingElement(name.clone()))?;
                if name == "fakesink" {
                    sink.set_property("sync", true);
                }
                sink
            }
        };

        let shared = Arc::new(Shared {
            next_uri: Mutex::new(None),
            next_provider: Mutex::new(None),
            latency_start: Mutex::new(None),
        });

        if let Some(pad) = sink.static_pad("sink") {
            let shared = shared.clone();
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                if let Some(start) = shared.latency_start.lock().unwrap().take() {
                    let ms = start.elapsed().as_millis();
                    tracing::info!(
                        play_latency_ms = ms as u64,
                        "first audio buffer reached the sink"
                    );
                }
                gst::PadProbeReturn::Ok
            });
        }
        playbin.set_property("audio-sink", &sink);

        let replaygain = match ReplayGainFilter::new() {
            Ok(rg) => {
                playbin.set_property("audio-filter", rg.element());
                Some(rg)
            }
            Err(e) => {
                tracing::warn!("ReplayGain elements unavailable, playing without: {e}");
                None
            }
        };

        {
            let shared = shared.clone();
            playbin.connect("about-to-finish", false, move |args| {
                let playbin = args[0].get::<gst::Element>().unwrap();
                let next = shared.next_uri.lock().unwrap().take().or_else(|| {
                    let provider = shared.next_provider.lock().unwrap().clone()?;
                    let current = playbin.property::<Option<String>>("current-uri");
                    provider(current.as_deref())
                });
                if let Some(uri) = next {
                    tracing::debug!(%uri, "gapless switch");
                    playbin.set_property("uri", &uri);
                }
                None
            });
        }

        Ok(Self {
            playbin,
            shared,
            replaygain,
            replaygain_mode: Mutex::new(ReplayGain::Track),
            _bus_watch: Mutex::new(None),
        })
    }

    pub fn playbin(&self) -> &gst::Element {
        &self.playbin
    }

    pub fn bus(&self) -> gst::Bus {
        self.playbin.bus().expect("pipeline has a bus")
    }

    /// Delivers [`PlayerEvent`]s on the thread-default GLib main context.
    pub fn connect_events<F: Fn(PlayerEvent) + 'static>(&self, f: F) {
        let playbin = self.playbin.clone();
        let guard = self
            .bus()
            .add_watch_local(move |_, msg| {
                if let Some(event) = translate(&playbin, msg) {
                    f(event);
                }
                gst::glib::ControlFlow::Continue
            })
            .expect("bus watch already installed");
        *self._bus_watch.lock().unwrap() = Some(guard);
    }

    /// Converts one bus message into an event (for callers that poll the bus themselves).
    pub fn translate_message(&self, msg: &gst::Message) -> Option<PlayerEvent> {
        translate(&self.playbin, msg)
    }

    /// The URI used for the next gapless switch, or `None` to end after the current track.
    pub fn set_next_uri(&self, uri: Option<String>) {
        *self.shared.next_uri.lock().unwrap() = uri;
    }

    /// Installs a callback consulted on `about-to-finish` when no next URI was set.
    pub fn set_next_provider(&self, provider: Option<NextProvider>) {
        *self.shared.next_provider.lock().unwrap() = provider;
    }

    /// Starts playing `uri` from the beginning (or from `start_ms`), replacing the current stream.
    pub fn play_uri(&self, uri: &str, start_ms: Option<u64>, paused: bool) -> Result<(), Error> {
        self.playbin.set_state(gst::State::Ready)?;
        self.apply_replaygain_mode();
        self.playbin.set_property("uri", uri);
        *self.shared.latency_start.lock().unwrap() = (!paused).then(Instant::now);
        if let Some(ms) = start_ms.filter(|ms| *ms > 0) {
            self.playbin.set_state(gst::State::Paused)?;
            let _ = self.playbin.state(gst::ClockTime::from_seconds(5));
            self.seek_ms(ms);
        }
        let target = if paused {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        self.playbin.set_state(target)?;
        Ok(())
    }

    pub fn play(&self) {
        let _ = self.playbin.set_state(gst::State::Playing);
    }

    pub fn pause(&self) {
        let _ = self.playbin.set_state(gst::State::Paused);
    }

    pub fn stop(&self) {
        self.set_next_uri(None);
        let _ = self.playbin.set_state(gst::State::Null);
    }

    pub fn is_playing(&self) -> bool {
        self.playbin.current_state() == gst::State::Playing
    }

    pub fn current_uri(&self) -> Option<String> {
        self.playbin.property::<Option<String>>("current-uri")
    }

    pub fn position_ms(&self) -> Option<u64> {
        self.playbin
            .query_position::<gst::ClockTime>()
            .map(|t| t.mseconds())
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.playbin
            .query_duration::<gst::ClockTime>()
            .map(|t| t.mseconds())
    }

    pub fn seek_ms(&self, ms: u64) -> bool {
        // Not ACCURATE: without a seek table (most MP3s, many FLACs) the parser would then
        // read every frame from its last indexed position up to the target, which on a
        // track that is still streaming means waiting for the download to get there.
        // Audio frames are short, so a plain seek lands within a few tens of ms anyway.
        self.playbin
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::from_mseconds(ms))
            .is_ok()
    }

    pub fn is_seekable(&self) -> bool {
        let mut query = gst::query::Seeking::new(gst::Format::Time);
        self.playbin.query(&mut query) && query.result().0
    }

    /// Volume on a cubic scale (0.0–1.0), as users perceive it.
    pub fn set_volume(&self, cubic: f64) {
        let linear = gst_audio::StreamVolume::convert_volume(
            gst_audio::StreamVolumeFormat::Cubic,
            gst_audio::StreamVolumeFormat::Linear,
            cubic.clamp(0.0, 1.0),
        );
        self.playbin.set_property("volume", linear);
    }

    pub fn volume(&self) -> f64 {
        let linear: f64 = self.playbin.property("volume");
        gst_audio::StreamVolume::convert_volume(
            gst_audio::StreamVolumeFormat::Linear,
            gst_audio::StreamVolumeFormat::Cubic,
            linear,
        )
    }

    /// Sets the ReplayGain mode. Switching to or from `Off` applies on the next track start.
    pub fn set_replaygain(&self, mode: ReplayGain) {
        *self.replaygain_mode.lock().unwrap() = mode;
        if let Some(rg) = &self.replaygain {
            rg.set_album_mode(mode == ReplayGain::Album);
        }
        if self.playbin.current_state() <= gst::State::Ready {
            self.apply_replaygain_mode();
        }
    }

    /// Gain to use when the current track has no ReplayGain tags.
    pub fn set_fallback_gain(&self, gain_db: Option<f64>) {
        if let Some(rg) = &self.replaygain {
            rg.set_fallback_gain(gain_db);
        }
    }

    fn apply_replaygain_mode(&self) {
        let Some(rg) = &self.replaygain else { return };
        let off = *self.replaygain_mode.lock().unwrap() == ReplayGain::Off;
        let current: Option<gst::Element> = self.playbin.property("audio-filter");
        match (off, current.is_some()) {
            (true, true) => self
                .playbin
                .set_property("audio-filter", None::<gst::Element>),
            (false, false) => self.playbin.set_property("audio-filter", rg.element()),
            _ => {}
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.playbin.set_state(gst::State::Null);
    }
}

fn translate(playbin: &gst::Element, msg: &gst::Message) -> Option<PlayerEvent> {
    use gst::MessageView;
    match msg.view() {
        MessageView::StateChanged(change) if msg.src() == Some(playbin.upcast_ref()) => {
            let state = match change.current() {
                gst::State::Playing => PlayerState::Playing,
                gst::State::Paused if change.pending() == gst::State::Playing => {
                    PlayerState::Loading
                }
                gst::State::Paused => PlayerState::Paused,
                gst::State::Ready => PlayerState::Stopped,
                gst::State::Null | gst::State::VoidPending => PlayerState::Idle,
            };
            Some(PlayerEvent::State(state))
        }
        MessageView::StreamStart(_) => {
            let uri = playbin
                .property::<Option<String>>("current-uri")
                .unwrap_or_default();
            Some(PlayerEvent::StreamStarted { uri })
        }
        MessageView::Eos(_) => Some(PlayerEvent::Eos),
        MessageView::Error(err) => {
            let uri = playbin.property::<Option<String>>("current-uri");
            tracing::warn!(?uri, "playback error: {} ({:?})", err.error(), err.debug());
            Some(PlayerEvent::Error {
                uri,
                message: err.error().to_string(),
            })
        }
        MessageView::DurationChanged(_) => Some(PlayerEvent::DurationChanged),
        MessageView::Buffering(b) if b.percent() < 100 => {
            Some(PlayerEvent::State(PlayerState::Buffering))
        }
        _ => None,
    }
}
