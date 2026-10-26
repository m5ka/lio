//! `PlaybackController`: owns the `Player`, the persistent play queue and the link to the
//! TrackStore.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::gio;
use gtk::glib;
use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use lio_playback::{Player, PlayerEvent, PlayerState, ReplayGain, TrackResolver, TrackSource};
use lio_store::queue::{Origin, Queue, QueueEntry, Repeat};
use lio_store::{Db, Quality, TrackHandle, TrackRow, TrackStore};

use crate::models::Track;
use crate::runtime;
use crate::settings::{ReplayGainMode, Settings};

/// Restart the current track instead of going back when past this position.
const PREVIOUS_RESTART_MS: u64 = 3000;
const POSITION_INTERVAL: Duration = Duration::from_millis(250);
const SEEK_STEP_MS: u64 = 10_000;
/// Queue writes are debounced to at most one per second.
const SAVE_DEBOUNCE: Duration = Duration::from_secs(1);

/// Adapts [`TrackStore`] to `liosrc`'s resolver interface.
struct StoreResolver(TrackStore);

struct HandleSource(TrackHandle);

impl TrackSource for HandleSource {
    fn size(&self) -> Option<u64> {
        self.0.size()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read_at(offset, buf)
    }
    fn cancel(&self) {
        self.0.cancel();
    }
    fn resume(&self) {
        self.0.resume();
    }
}

impl TrackResolver for StoreResolver {
    fn open(&self, track_id: &str, quality: &str) -> Result<Box<dyn TrackSource>, String> {
        let quality = Quality::parse(quality).unwrap_or_default();
        self.0
            .open_by_id(track_id, quality)
            .map(|h| Box::new(HandleSource(h)) as Box<dyn TrackSource>)
            .map_err(|e| e.to_string())
    }
}

/// Where the queue came from; drives ReplayGain "auto" (album gain for whole albums in order).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum QueueOrigin {
    #[default]
    None,
    Album(String),
    Playlist(String),
    Artist(String),
    Search,
}

impl QueueOrigin {
    fn to_origin(&self) -> Origin {
        let (kind, id) = match self {
            Self::None => (None, None),
            Self::Album(id) => (Some("album"), Some(id.clone())),
            Self::Playlist(id) => (Some("playlist"), Some(id.clone())),
            Self::Artist(id) => (Some("artist"), Some(id.clone())),
            Self::Search => (Some("search"), None),
        };
        Origin {
            kind: kind.map(str::to_owned),
            id,
        }
    }
}

/// The gapless "next" the player was told about.
#[derive(Debug, Clone)]
pub struct PendingNext {
    uri: String,
    position: usize,
}

mod imp {
    use std::sync::LazyLock;

    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::PlaybackController)]
    pub struct PlaybackController {
        pub player: RefCell<Option<Rc<Player>>>,
        pub store: RefCell<Option<TrackStore>>,
        pub db: RefCell<Option<Db>>,
        pub settings: RefCell<Option<Settings>>,
        pub queue: RefCell<Queue>,
        /// Tracks in play order, for the queue sidebar.
        pub queue_model: RefCell<Option<gio::ListStore>>,
        pub prefetched: Cell<bool>,
        /// Views showing the spectrum; analysis runs while there is at least one.
        pub spectrum_users: Cell<u32>,
        /// Views using raw samples; copying runs while there is at least one.
        pub pcm_users: Cell<u32>,
        pub ticker: RefCell<Option<glib::SourceId>>,
        pub save_pending: Cell<bool>,
        /// URI of the stream the controller considers current.
        pub current_uri: RefCell<Option<String>>,
        pub pending_next: RefCell<Option<PendingNext>>,
        pub uri_seq: Cell<u64>,

        #[property(get, nullable)]
        pub current_track: RefCell<Option<Track>>,
        #[property(get)]
        pub playing: Cell<bool>,
        /// Something is loaded (playing or paused).
        #[property(get)]
        pub active: Cell<bool>,
        #[property(get)]
        pub position_ms: Cell<u64>,
        #[property(get)]
        pub duration_ms: Cell<u64>,
        #[property(get, set = Self::set_volume, minimum = 0.0, maximum = 1.0, default = 1.0)]
        pub volume: Cell<f64>,
        #[property(get)]
        pub has_next: Cell<bool>,
        #[property(get)]
        pub has_previous: Cell<bool>,
        #[property(get)]
        pub seekable: Cell<bool>,
        #[property(get, set = Self::set_shuffle)]
        pub shuffle: Cell<bool>,
        /// `off`, `all` or `one`.
        #[property(get, set = Self::set_repeat)]
        pub repeat: RefCell<String>,
    }

    impl PlaybackController {
        fn set_volume(&self, volume: f64) {
            self.volume.set(volume);
            if let Some(player) = self.player.borrow().as_ref() {
                player.set_volume(volume);
            }
            if let Some(settings) = self.settings.borrow().as_ref() {
                let _ = settings.set_double("volume", volume);
            }
        }

        fn set_shuffle(&self, shuffle: bool) {
            self.shuffle.set(shuffle);
            self.queue.borrow_mut().set_shuffle(shuffle);
            self.obj().queue_changed();
        }

        fn set_repeat(&self, repeat: String) {
            let mode = Repeat::parse(&repeat);
            self.repeat.replace(mode.as_str().to_owned());
            self.queue.borrow_mut().set_repeat(mode);
            self.obj().queue_changed();
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlaybackController {
        const NAME: &'static str = "LioPlaybackController";
        type Type = super::PlaybackController;
    }

    #[glib::derived_properties]
    impl ObjectImpl for PlaybackController {
        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    // A user-facing problem (unplayable track, offline, ...).
                    Signal::builder("error")
                        .param_types([String::static_type()])
                        .build(),
                    // A new track started (for scrobbling and notifications).
                    Signal::builder("track-started").build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }
}

glib::wrapper! {
    pub struct PlaybackController(ObjectSubclass<imp::PlaybackController>);
}

impl PlaybackController {
    pub fn new(settings: &Settings) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.settings.replace(Some(settings.clone()));
        imp.volume.set(settings.volume());
        imp.repeat.replace("off".into());
        imp.queue_model
            .replace(Some(gio::ListStore::new::<Track>()));
        match Player::new(None) {
            Ok(player) => {
                player.set_volume(settings.volume());
                player.set_fade_duration(settings.pause_fade());
                let player = Rc::new(player);
                player.connect_events(glib::clone!(
                    #[weak]
                    obj,
                    move |event| obj.handle_event(event)
                ));
                imp.player.replace(Some(player));
            }
            Err(e) => tracing::error!("audio playback unavailable: {e}"),
        }
        for key in ["fade-on-pause", "fade-duration"] {
            settings.connect_changed_key(
                key,
                glib::clone!(
                    #[weak]
                    obj,
                    move |settings| {
                        if let Some(player) = obj.player() {
                            player.set_fade_duration(settings.pause_fade());
                        }
                    }
                ),
            );
        }
        settings.connect_changed_key(
            "replaygain",
            glib::clone!(
                #[weak]
                obj,
                move |_| obj.apply_replaygain()
            ),
        );
        obj
    }

    /// Attaches (or detaches) the account's TrackStore and database, restoring the
    /// saved queue paused at its saved position.
    pub fn set_store(&self, store: Option<TrackStore>) {
        let imp = self.imp();
        match &store {
            Some(store) => {
                lio_playback::set_resolver(Arc::new(StoreResolver(store.clone())));
                let same = imp
                    .store
                    .borrow()
                    .as_ref()
                    .is_some_and(|s| s.paths().account == store.paths().account);
                imp.db.replace(Some(store.db().clone()));
                imp.store.replace(Some(store.clone()));
                if !same {
                    self.restore();
                }
            }
            None => {
                self.save_now();
                self.stop();
                imp.queue.replace(Queue::new());
                imp.store.replace(None);
                imp.db.replace(None);
                imp.current_track.replace(None);
                self.notify_current_track();
                self.queue_changed();
            }
        }
    }

    fn restore(&self) {
        let imp = self.imp();
        let Some(db) = imp.db.borrow().clone() else {
            return;
        };
        let queue = match db.read(Queue::load) {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!("could not restore the queue: {e}");
                return;
            }
        };
        let (shuffle, repeat, position) = (queue.shuffle(), queue.repeat(), queue.position_ms);
        let cursor = queue.cursor();
        imp.queue.replace(queue);
        imp.shuffle.set(shuffle);
        imp.repeat.replace(repeat.as_str().to_owned());
        self.notify_shuffle();
        self.notify_repeat();
        self.queue_changed();
        if let Some(pos) = cursor {
            tracing::info!(
                tracks = imp.queue.borrow().len(),
                position_ms = position,
                "restored queue"
            );
            self.play_position(pos, Some(position), true);
        }
    }

    fn player(&self) -> Option<Rc<Player>> {
        self.imp().player.borrow().clone()
    }

    /// Starts or stops wanting spectrum analysis for one view. Counted, so a view going
    /// away doesn't stop it for another that has just appeared (e.g. while the Now
    /// Playing views crossfade).
    pub fn want_spectrum(&self, wanted: bool) {
        let users = &self.imp().spectrum_users;
        let count = if wanted {
            users.get() + 1
        } else {
            users.get().saturating_sub(1)
        };
        users.set(count);
        if let Some(player) = self.player() {
            player.set_spectrum_enabled(count > 0);
        }
    }

    /// Starts or stops wanting raw samples for one view; counted like
    /// [`want_spectrum`](Self::want_spectrum).
    pub fn want_pcm(&self, wanted: bool) {
        let users = &self.imp().pcm_users;
        let count = if wanted {
            users.get() + 1
        } else {
            users.get().saturating_sub(1)
        };
        users.set(count);
        if let Some(player) = self.player() {
            player.set_pcm_enabled(count > 0);
        }
    }

    /// Interleaved stereo samples heard since the last call, while wanted.
    pub fn pcm(&self) -> Vec<f32> {
        self.player().map_or_else(Vec::new, |player| player.pcm())
    }

    /// The spectrum of what is being heard now, while analysis is on.
    pub fn spectrum(&self) -> Option<lio_playback::SpectrumFrame> {
        self.player()?.spectrum()
    }

    fn settings(&self) -> Settings {
        self.imp().settings.borrow().clone().expect("settings")
    }

    /// Tracks in play order (for the queue sidebar).
    pub fn queue_model(&self) -> gio::ListStore {
        self.imp()
            .queue_model
            .borrow()
            .clone()
            .expect("created in new()")
    }

    pub fn queue_rows(&self) -> Vec<TrackRow> {
        self.imp()
            .queue
            .borrow()
            .items()
            .into_iter()
            .map(|e| e.track.clone())
            .collect()
    }

    /// Position of the current track in play order.
    pub fn current_position(&self) -> Option<usize> {
        self.imp().queue.borrow().cursor()
    }

    /// Quality for streaming right now (metered vs unmetered).
    pub fn stream_quality(&self) -> Quality {
        let metered = gio::NetworkMonitor::default().is_network_metered();
        self.settings().stream_quality(metered)
    }

    fn entries(tracks: Vec<TrackRow>, origin: &QueueOrigin) -> Vec<QueueEntry> {
        let origin = origin.to_origin();
        tracks
            .into_iter()
            .map(|t| QueueEntry::new(t, origin.clone()))
            .collect()
    }

    /// Replaces the queue and starts playing at `start`.
    pub fn play_tracks(&self, tracks: Vec<TrackRow>, start: usize, origin: QueueOrigin) {
        if tracks.is_empty() {
            return;
        }
        let imp = self.imp();
        imp.queue
            .borrow_mut()
            .set(Self::entries(tracks, &origin), start);
        let cursor = imp.queue.borrow().cursor().unwrap_or(0);
        self.queue_changed();
        self.play_position(cursor, None, false);
    }

    /// Turns shuffle on and plays `tracks` from a random one (the Shuffle buttons on
    /// album, artist and playlist pages). The queue keeps the original order, so turning
    /// shuffle off continues in album or playlist order.
    pub fn shuffle_tracks(&self, tracks: Vec<TrackRow>, origin: QueueOrigin) {
        if tracks.is_empty() {
            return;
        }
        let start = glib::random_int_range(0, tracks.len() as i32) as usize;
        let imp = self.imp();
        if !imp.shuffle.replace(true) {
            imp.queue.borrow_mut().set_shuffle(true);
            self.notify_shuffle();
        }
        self.play_tracks(tracks, start, origin);
    }

    /// Inserts tracks after the current one.
    pub fn play_next(&self, tracks: Vec<TrackRow>) {
        let was_empty = self.imp().queue.borrow().is_empty();
        self.imp()
            .queue
            .borrow_mut()
            .play_next(Self::entries(tracks, &QueueOrigin::None));
        self.queue_changed();
        if was_empty {
            self.play_position(0, None, false);
        }
    }

    /// Appends tracks to the queue.
    pub fn enqueue(&self, tracks: Vec<TrackRow>) {
        let was_empty = self.imp().queue.borrow().is_empty();
        self.imp()
            .queue
            .borrow_mut()
            .enqueue(Self::entries(tracks, &QueueOrigin::None));
        self.queue_changed();
        if was_empty {
            self.play_position(0, None, false);
        }
    }

    pub fn move_item(&self, from: usize, to: usize) {
        self.imp().queue.borrow_mut().move_item(from, to);
        self.queue_changed();
    }

    /// Removes the track at play position `pos`; removing the current track plays the
    /// one that takes its place.
    pub fn remove_at(&self, pos: usize) {
        let imp = self.imp();
        let was_current = imp.queue.borrow().cursor() == Some(pos);
        imp.queue.borrow_mut().remove(pos);
        self.queue_changed();
        if was_current {
            let cursor = imp.queue.borrow().cursor();
            match cursor {
                Some(c) => self.play_position(c, None, !imp.playing.get()),
                None => self.stop(),
            }
        }
    }

    pub fn clear_queue(&self) {
        self.stop();
        let imp = self.imp();
        imp.queue.borrow_mut().clear();
        imp.current_track.replace(None);
        self.notify_current_track();
        self.queue_changed();
    }

    /// Plays the track at play position `pos`.
    pub fn play_position(&self, pos: usize, start_ms: Option<u64>, paused: bool) {
        let imp = self.imp();
        let Some(pos) = self.first_playable_from(pos) else {
            self.emit_error(&gettextrs::gettext(
                "Nothing in the queue is available offline",
            ));
            self.stop();
            return;
        };
        let Some(entry) = imp.queue.borrow_mut().select(pos).cloned() else {
            return;
        };
        let Some(player) = self.player() else {
            self.emit_error(&gettextrs::gettext("Audio playback isn't available"));
            return;
        };
        let uri = self.next_uri(&entry.track);
        imp.current_uri.replace(Some(uri.clone()));
        imp.pending_next.replace(None);
        if let Some(player) = self.player() {
            player.set_next_uri(None);
        }
        imp.prefetched.set(false);
        self.set_current(&entry.track);
        self.apply_replaygain();
        player.set_fallback_gain(fallback_gain(&entry.track, self.album_gain_mode()));
        if let Err(e) = player.play_uri(&uri, start_ms, paused) {
            tracing::error!("cannot start playback: {e}");
            self.emit_error(&gettextrs::gettext("Can't play this track"));
            return;
        }
        if let Some(ms) = start_ms {
            imp.position_ms.set(ms);
            self.notify_position_ms();
        }
        self.refresh_next();
        self.ensure_ticker();
        self.emit_by_name::<()>("track-started", &[]);
    }

    /// `pos`, or the next position whose track can play right now: offline, tracks
    /// without a local copy are skipped with a toast.
    fn first_playable_from(&self, pos: usize) -> Option<usize> {
        let imp = self.imp();
        let store = imp.store.borrow().clone();
        let Some(store) = store.filter(|s| !s.is_online()) else {
            return Some(pos);
        };
        let queue = imp.queue.borrow();
        let len = queue.len();
        let playable = |p: usize| {
            queue.get(p).is_some_and(|e| {
                store
                    .resolve_local(e.track.rowid, Quality::Original)
                    .ok()
                    .flatten()
                    .is_some()
            })
        };
        let found = (pos..len).find(|p| playable(*p));
        if found.is_some_and(|f| f != pos) {
            self.emit_error(&gettextrs::gettext(
                "Skipped songs that aren't available offline",
            ));
        }
        found
    }

    fn next_uri(&self, track: &TrackRow) -> String {
        let seq = self.imp().uri_seq.get() + 1;
        self.imp().uri_seq.set(seq);
        lio_playback::track_uri_keyed(&track.id, self.stream_quality().as_str(), seq)
    }

    fn set_current(&self, row: &TrackRow) {
        let imp = self.imp();
        let track = Track::new(row.clone());
        track.set_playing(true);
        if let Some(old) = imp.current_track.replace(Some(track)) {
            old.set_playing(false);
        }
        imp.position_ms.set(0);
        imp.duration_ms.set(row.duration_s.max(0) as u64 * 1000);
        self.notify_current_track();
        self.notify_position_ms();
        self.notify_duration_ms();
        self.schedule_save();
    }

    /// Rebuilds the queue model and refreshes next/previous state.
    fn queue_changed(&self) {
        let imp = self.imp();
        let tracks: Vec<Track> = imp
            .queue
            .borrow()
            .items()
            .into_iter()
            .map(|e| Track::new(e.track.clone()))
            .collect();
        let current = imp.queue.borrow().cursor();
        if let Some(i) = current
            && let Some(t) = tracks.get(i)
        {
            t.set_playing(true);
        }
        let model = self.queue_model();
        model.splice(0, model.n_items(), &tracks);
        self.refresh_next();
        self.schedule_save();
    }

    /// Recomputes the gapless next URI, `has-next/previous` and eviction protection.
    fn refresh_next(&self) {
        let imp = self.imp();
        let (next_pos, has_previous, current_rowid) = {
            let queue = imp.queue.borrow();
            (
                queue.peek_next(true),
                queue.peek_previous().is_some() || queue.cursor().is_some(),
                queue.current().map(|e| e.track.rowid),
            )
        };
        let next = next_pos.and_then(|p| imp.queue.borrow().get(p).map(|e| (p, e.track.clone())));
        // Keep the URI when the upcoming track didn't change: playbin may already have
        // taken it in `about-to-finish`, and the switch is recognised by URI.
        let previous = imp.pending_next.borrow().clone();
        let pending = next.as_ref().map(|(pos, row)| match previous {
            Some(p)
                if p.position == *pos
                    && lio_playback::parse_track_uri(&p.uri)
                        .is_some_and(|(id, _)| id == row.id) =>
            {
                p
            }
            _ => PendingNext {
                uri: self.next_uri(row),
                position: *pos,
            },
        });
        if let Some(player) = self.player() {
            player.set_next_uri(pending.as_ref().map(|p| p.uri.clone()));
        }
        imp.pending_next.replace(pending);
        let has_next = imp.queue.borrow().peek_next(false).is_some();
        if let Some(store) = imp.store.borrow().as_ref() {
            store.set_protected(current_rowid.into_iter().chain(next.map(|(_, r)| r.rowid)));
        }
        if imp.has_next.replace(has_next) != has_next {
            self.notify_has_next();
        }
        if imp.has_previous.replace(has_previous) != has_previous {
            self.notify_has_previous();
        }
    }

    fn handle_event(&self, event: PlayerEvent) {
        let imp = self.imp();
        match event {
            PlayerEvent::State(state) => {
                let playing = state == PlayerState::Playing;
                let active = matches!(
                    state,
                    PlayerState::Playing
                        | PlayerState::Paused
                        | PlayerState::Loading
                        | PlayerState::Buffering
                );
                if imp.playing.replace(playing) != playing {
                    self.notify_playing();
                }
                if imp.active.replace(active) != active {
                    self.notify_active();
                }
                if active {
                    let seekable = self.player().is_some_and(|p| p.is_seekable());
                    if imp.seekable.replace(seekable) != seekable {
                        self.notify_seekable();
                    }
                }
            }
            PlayerEvent::StreamStarted { uri } => {
                let pending = imp.pending_next.borrow().clone();
                if let Some(next) = pending.filter(|p| p.uri == uri) {
                    // Gapless switch to the next queue item.
                    let entry = imp.queue.borrow_mut().select(next.position).cloned();
                    if let Some(entry) = entry {
                        imp.current_uri.replace(Some(uri));
                        imp.prefetched.set(false);
                        self.set_current(&entry.track);
                        if let Some(player) = self.player() {
                            player.set_fallback_gain(fallback_gain(
                                &entry.track,
                                self.album_gain_mode(),
                            ));
                        }
                        self.queue_changed();
                        self.emit_by_name::<()>("track-started", &[]);
                    }
                }
            }
            PlayerEvent::Eos => {
                tracing::debug!("end of queue");
                if let Some(player) = self.player() {
                    player.stop();
                }
                imp.position_ms.set(0);
                imp.queue.borrow_mut().position_ms = 0;
                self.notify_position_ms();
                if imp.playing.replace(false) {
                    self.notify_playing();
                }
                if imp.active.replace(false) {
                    self.notify_active();
                }
                self.schedule_save();
            }
            PlayerEvent::Error { message, .. } => {
                tracing::warn!("skipping unplayable track: {message}");
                self.emit_error(&gettextrs::gettext("Can't play this track, skipping it"));
                let (next, cursor) = {
                    let queue = imp.queue.borrow();
                    (queue.peek_next(false), queue.cursor())
                };
                match next {
                    Some(n) if Some(n) != cursor => self.play_position(n, None, false),
                    _ => self.stop(),
                }
            }
            PlayerEvent::DurationChanged => {
                if let Some(ms) = self.player().and_then(|p| p.duration_ms()) {
                    imp.duration_ms.set(ms);
                    self.notify_duration_ms();
                }
            }
        }
    }

    fn ensure_ticker(&self) {
        let imp = self.imp();
        if imp.ticker.borrow().is_some() {
            return;
        }
        let id = glib::timeout_add_local(
            POSITION_INTERVAL,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    this.tick();
                    glib::ControlFlow::Continue
                }
            ),
        );
        imp.ticker.replace(Some(id));
    }

    fn tick(&self) {
        let imp = self.imp();
        let Some(player) = self.player() else { return };
        if !imp.playing.get() {
            return;
        }
        if let Some(pos) = player.position_ms() {
            imp.position_ms.set(pos);
            imp.queue.borrow_mut().position_ms = pos;
            self.notify_position_ms();
            let duration = imp.duration_ms.get();
            // Prefetch the next track at 50% or 30 s before the end.
            if !imp.prefetched.get()
                && duration > 0
                && (pos * 2 >= duration || duration.saturating_sub(pos) <= 30_000)
            {
                imp.prefetched.set(true);
                self.prefetch_next();
            }
            if pos % 5000 < POSITION_INTERVAL.as_millis() as u64 {
                self.schedule_save();
            }
        }
    }

    fn prefetch_next(&self) {
        let imp = self.imp();
        let next = imp
            .queue
            .borrow()
            .peek_next(true)
            .and_then(|p| imp.queue.borrow().get(p).map(|e| e.track.rowid));
        let (Some(rowid), Some(store)) = (next, imp.store.borrow().clone()) else {
            return;
        };
        let quality = self.stream_quality();
        runtime::spawn(async move {
            let result = tokio::task::spawn_blocking(move || store.prefetch(rowid, quality)).await;
            if let Ok(Err(e)) = result {
                tracing::debug!(rowid, "prefetch skipped: {e}");
            }
        });
    }

    // Persistence ---------------------------------------------------------------------

    fn schedule_save(&self) {
        let imp = self.imp();
        if imp.save_pending.replace(true) {
            return;
        }
        glib::timeout_add_local_once(
            SAVE_DEBOUNCE,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.imp().save_pending.set(false);
                    let Some(db) = this.imp().db.borrow().clone() else {
                        return;
                    };
                    let queue = this.imp().queue.borrow().clone();
                    runtime::spawn(async move {
                        if let Err(e) = db.run(move |c| queue.save(c)).await {
                            tracing::warn!("could not save the queue: {e}");
                        }
                    });
                }
            ),
        );
    }

    /// Saves synchronously (quit, sign-out).
    pub fn save_now(&self) {
        let imp = self.imp();
        let Some(db) = imp.db.borrow().clone() else {
            return;
        };
        let queue = imp.queue.borrow().clone();
        if let Err(e) = db.with(|c| queue.save(c)) {
            tracing::warn!("could not save the queue: {e}");
        }
    }

    // ReplayGain ------------------------------------------------------------------------

    fn album_gain_mode(&self) -> bool {
        match self.settings().replaygain() {
            ReplayGainMode::Album => true,
            ReplayGainMode::Auto => {
                let queue = self.imp().queue.borrow();
                !queue.shuffle()
                    && queue
                        .current()
                        .is_some_and(|e| e.origin.kind.as_deref() == Some("album"))
            }
            _ => false,
        }
    }

    fn apply_replaygain(&self) {
        let Some(player) = self.player() else { return };
        let mode = match self.settings().replaygain() {
            ReplayGainMode::Off => ReplayGain::Off,
            _ if self.album_gain_mode() => ReplayGain::Album,
            _ => ReplayGain::Track,
        };
        player.set_replaygain(mode);
    }

    fn emit_error(&self, message: &str) {
        self.emit_by_name::<()>("error", &[&message]);
    }

    pub fn connect_error<F: Fn(&Self, &str) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "error",
            false,
            glib::closure_local!(move |obj: &Self, message: String| f(obj, &message)),
        )
    }

    pub fn connect_track_started<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "track-started",
            false,
            glib::closure_local!(move |obj: &Self| f(obj)),
        )
    }

    // Transport ---------------------------------------------------------------------

    pub fn play_pause(&self) {
        let Some(player) = self.player() else { return };
        let imp = self.imp();
        if imp.playing.get() {
            self.pause();
        } else if imp.active.get() {
            // Resuming during a fade-out: the pipeline never stopped, so no state change
            // will report it; show it as playing now.
            let was_fading_out = player.is_fading_out();
            player.play();
            if was_fading_out && !imp.playing.replace(true) {
                self.notify_playing();
            }
        } else {
            let cursor = imp.queue.borrow().cursor();
            if let Some(c) = cursor {
                self.play_position(c, Some(imp.position_ms.get()), false);
            }
        }
    }

    pub fn play(&self) {
        if !self.imp().playing.get() {
            self.play_pause();
        }
    }

    pub fn pause(&self) {
        if let Some(player) = self.player() {
            player.pause();
            // With a fade, the pipeline pauses only when it ends; show it paused now.
            if player.is_fading_out() && self.imp().playing.replace(false) {
                self.notify_playing();
            }
        }
    }

    pub fn stop(&self) {
        if let Some(player) = self.player() {
            player.stop();
        }
        let imp = self.imp();
        if imp.active.replace(false) {
            self.notify_active();
        }
        if imp.playing.replace(false) {
            self.notify_playing();
        }
    }

    pub fn next(&self) {
        let imp = self.imp();
        let next = imp.queue.borrow().peek_next(false);
        if let Some(n) = next {
            self.play_position(n, None, !imp.playing.get() && imp.active.get());
        }
    }

    pub fn previous(&self) {
        let imp = self.imp();
        let prev = imp.queue.borrow().peek_previous();
        match prev {
            Some(p) if imp.position_ms.get() <= PREVIOUS_RESTART_MS => {
                self.play_position(p, None, !imp.playing.get() && imp.active.get());
            }
            _ => self.seek_ms(0),
        }
    }

    pub fn seek_ms(&self, ms: u64) {
        if let Some(player) = self.player()
            && player.seek_ms(ms)
        {
            self.imp().position_ms.set(ms);
            self.notify_position_ms();
        }
    }

    pub fn seek_relative(&self, forward: bool) {
        let pos = self.imp().position_ms.get();
        let target = if forward {
            pos + SEEK_STEP_MS
        } else {
            pos.saturating_sub(SEEK_STEP_MS)
        };
        self.seek_ms(target.min(self.imp().duration_ms.get()));
    }

    pub fn change_volume(&self, delta: f64) {
        self.set_volume((self.volume() + delta).clamp(0.0, 1.0));
    }

    /// off → all → one → off.
    pub fn cycle_repeat(&self) {
        let next = Repeat::parse(&self.repeat()).cycle();
        self.set_repeat(next.as_str());
    }
}

/// Server-provided gain for tracks whose files carry no ReplayGain tags.
fn fallback_gain(row: &TrackRow, album_mode: bool) -> Option<f64> {
    if album_mode {
        row.rg_album_gain.or(row.rg_track_gain)
    } else {
        row.rg_track_gain.or(row.rg_album_gain)
    }
}
