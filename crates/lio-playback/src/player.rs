//! `Player`: playbin3 wrapper with gapless track switching.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gst::prelude::*;

use crate::pcm::Pcm;
use crate::replaygain::{ReplayGain, ReplayGainFilter};
use crate::spectrum::{Spectrum, SpectrumFrame};

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

/// How often a fade updates the volume.
const FADE_STEP: Duration = Duration::from_millis(10);

/// Pause/resume fade. Its level scales the user's volume, so the volume the user set
/// is never touched.
struct Fade {
    duration: Duration,
    /// 0.0 (silent) to 1.0 (full), linear in time; applied cubed, so it sounds even.
    level: f64,
    /// The running ramp, if any.
    timer: Option<gst::glib::Source>,
    /// State to switch to when the running ramp ends (`Paused` for a fade-out).
    then: Option<gst::State>,
}

pub struct Player {
    playbin: gst::Element,
    shared: Arc<Shared>,
    /// The user's volume, linear.
    volume: Arc<Mutex<f64>>,
    fade: Arc<Mutex<Fade>>,
    /// `None` if the `spectrum` element is missing.
    spectrum: Option<Arc<Spectrum>>,
    /// Raw samples for sample-based visualisers; `None` if elements are missing.
    pcm: Option<Pcm>,
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
        let spectrum = match Spectrum::new() {
            Ok(spectrum) => Some(Arc::new(spectrum)),
            Err(e) => {
                tracing::warn!("spectrum element unavailable, no visualiser: {e}");
                None
            }
        };
        let pcm = match Pcm::new() {
            Ok(pcm) => Some(pcm),
            Err(e) => {
                tracing::warn!("cannot copy audio for visualisers: {e}");
                None
            }
        };
        let sink = audio_sink_bin(&sink, spectrum.as_deref(), pcm.as_ref())?;
        playbin.set_property("audio-sink", &sink);
        if let Some(spectrum) = spectrum.clone() {
            // Taken off the bus on the streaming thread: up to 60 a second, not for the
            // main loop.
            playbin
                .bus()
                .expect("pipeline has a bus")
                .set_sync_handler(move |_, msg| {
                    if spectrum.handle_message(msg) {
                        gst::BusSyncReply::Drop
                    } else {
                        gst::BusSyncReply::Pass
                    }
                });
        }

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
            volume: Arc::new(Mutex::new(1.0)),
            fade: Arc::new(Mutex::new(Fade {
                duration: Duration::ZERO,
                level: 1.0,
                timer: None,
                then: None,
            })),
            spectrum,
            pcm,
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
        self.reset_fade();
        self.clear_spectrum();
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

    /// Resumes, fading in if a fade duration is set. Called during a fade-out, it
    /// cancels the pause and fades back in from the current level.
    pub fn play(&self) {
        if self.playbin.current_state() != gst::State::Playing {
            // Start silent; the ramp below brings it up (at once without a fade).
            self.fade.lock().unwrap().level = 0.0;
            self.apply_volume();
        }
        let _ = self.playbin.set_state(gst::State::Playing);
        self.ramp(1.0, None);
    }

    /// Pauses, fading out first if a fade duration is set.
    pub fn pause(&self) {
        if self.playbin.current_state() == gst::State::Playing {
            self.ramp(0.0, Some(gst::State::Paused));
        } else {
            self.reset_fade();
            let _ = self.playbin.set_state(gst::State::Paused);
        }
    }

    pub fn stop(&self) {
        self.set_next_uri(None);
        self.reset_fade();
        self.clear_spectrum();
        let _ = self.playbin.set_state(gst::State::Null);
    }

    /// How long [`pause`](Self::pause) fades out and [`play`](Self::play) fades in;
    /// zero pauses and resumes at once.
    pub fn set_fade_duration(&self, duration: Duration) {
        self.fade.lock().unwrap().duration = duration;
    }

    /// Whether a fade-out is running (the pipeline keeps playing until it ends).
    pub fn is_fading_out(&self) -> bool {
        let fade = self.fade.lock().unwrap();
        fade.timer.is_some() && fade.then == Some(gst::State::Paused)
    }

    /// Stops any fade and returns to full volume.
    fn reset_fade(&self) {
        let mut fade = self.fade.lock().unwrap();
        if let Some(timer) = fade.timer.take() {
            timer.destroy();
        }
        fade.level = 1.0;
        fade.then = None;
        drop(fade);
        self.apply_volume();
    }

    /// Moves the fade level to `to` over the fade duration (scaled by the distance
    /// left), then switches the pipeline to `then`, if given.
    fn ramp(&self, to: f64, then: Option<gst::State>) {
        let mut fade = self.fade.lock().unwrap();
        if let Some(timer) = fade.timer.take() {
            timer.destroy();
        }
        fade.then = None;
        let from = fade.level;
        let total = fade.duration.mul_f64((to - from).abs());
        if total < FADE_STEP {
            fade.level = to;
            drop(fade);
            self.apply_volume();
            if let Some(state) = then {
                let _ = self.playbin.set_state(state);
            }
            return;
        }
        fade.then = then;
        let start = Instant::now();
        let (playbin, volume, shared_fade) =
            (self.playbin.clone(), self.volume.clone(), self.fade.clone());
        // On the thread-default context, like the bus watch in `connect_events`.
        let source = gst::glib::timeout_source_new(
            FADE_STEP,
            Some("lio-fade"),
            gst::glib::Priority::DEFAULT,
            move || {
                let progress = (start.elapsed().as_secs_f64() / total.as_secs_f64()).min(1.0);
                let mut fade = shared_fade.lock().unwrap();
                fade.level = from + (to - from) * progress;
                set_playbin_volume(&playbin, *volume.lock().unwrap(), fade.level);
                if progress < 1.0 {
                    return gst::glib::ControlFlow::Continue;
                }
                // The source ends by returning Break.
                fade.timer = None;
                let then = fade.then.take();
                drop(fade);
                if let Some(state) = then {
                    let _ = playbin.set_state(state);
                }
                gst::glib::ControlFlow::Break
            },
        );
        source.attach(Some(&gst::glib::MainContext::ref_thread_default()));
        fade.timer = Some(source);
    }

    fn apply_volume(&self) {
        let level = self.fade.lock().unwrap().level;
        set_playbin_volume(&self.playbin, *self.volume.lock().unwrap(), level);
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
        self.clear_spectrum();
        self.playbin
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::from_mseconds(ms))
            .is_ok()
    }

    /// Turns spectrum analysis on or off (off by default; it costs an FFT per frame).
    pub fn set_spectrum_enabled(&self, enabled: bool) {
        if let Some(spectrum) = &self.spectrum {
            spectrum.set_enabled(enabled);
        }
    }

    /// The spectrum of the audio being heard now, once analysis is on and has caught up.
    pub fn spectrum(&self) -> Option<SpectrumFrame> {
        self.spectrum
            .as_ref()?
            .current(self.playbin.current_running_time())
    }

    /// Turns copying samples for [`pcm`](Self::pcm) on or off (off by default).
    pub fn set_pcm_enabled(&self, enabled: bool) {
        if let Some(pcm) = &self.pcm {
            pcm.set_enabled(enabled);
        }
    }

    /// Interleaved stereo float samples heard since the last call, while copying is on.
    pub fn pcm(&self) -> Vec<f32> {
        self.pcm.as_ref().map_or_else(Vec::new, |pcm| {
            pcm.take(self.playbin.current_running_time())
        })
    }

    /// Forgets queued analysis (spectrum and samples): seek, new track, stop.
    fn clear_spectrum(&self) {
        if let Some(spectrum) = &self.spectrum {
            spectrum.clear();
        }
        if let Some(pcm) = &self.pcm {
            pcm.clear();
        }
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
        *self.volume.lock().unwrap() = linear;
        self.apply_volume();
    }

    /// The user's volume (cubic), regardless of any fade in progress.
    pub fn volume(&self) -> f64 {
        let linear = *self.volume.lock().unwrap();
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

/// The audio sink with the analysis branches in front of it:
/// `tee ! queue ! [spectrum !] sink`, plus `tee ! <pcm branch>`.
fn audio_sink_bin(
    sink: &gst::Element,
    spectrum: Option<&Spectrum>,
    pcm: Option<&Pcm>,
) -> Result<gst::Element, Error> {
    if spectrum.is_none() && pcm.is_none() {
        return Ok(sink.clone());
    }
    let bin = gst::Bin::with_name("lio-audio-sink");
    let mut main: Vec<&gst::Element> = spectrum.map(Spectrum::element).into_iter().collect();
    main.push(sink);
    let entry = match pcm {
        Some(pcm) => {
            let tee = gst::ElementFactory::make("tee").build()?;
            // Short, so the analysis branches see audio only just before it is heard.
            let queue = gst::ElementFactory::make("queue")
                .property("max-size-time", 100_000_000u64)
                .property("max-size-buffers", 0u32)
                .property("max-size-bytes", 0u32)
                .build()?;
            bin.add_many([&tee, &queue])?;
            bin.add_many(main.iter().copied())?;
            bin.add_many(pcm.elements())?;
            gst::Element::link_many([&tee, &queue].into_iter().chain(main.iter().copied()))?;
            gst::Element::link_many(std::iter::once(&tee).chain(pcm.elements()))?;
            tee
        }
        None => {
            bin.add_many(main.iter().copied())?;
            gst::Element::link_many(main.iter().copied())?;
            main[0].clone()
        }
    };
    let pad = entry.static_pad("sink").expect("sink pad");
    bin.add_pad(&gst::GhostPad::with_target(&pad)?)?;
    Ok(bin.upcast())
}

/// Sets playbin's volume to the user's (linear) volume scaled by the fade level.
/// With PulseAudio/PipeWire sinks this is applied by the sound server, so a fade is
/// heard right away rather than after the sink's buffer.
fn set_playbin_volume(playbin: &gst::Element, volume: f64, level: f64) {
    playbin.set_property("volume", volume * level.powi(3));
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
