//! Wires playback to scrobbling: tracks listening time, reports "now playing",
//! queues completed plays and submits them with exponential back-off.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use lio_store::scrobble::{self, PlayTracker};

use crate::application::LioApplication;
use crate::playback_controller::PlaybackController;
use crate::runtime;
use crate::session::SessionState;

const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

#[derive(Default)]
struct State {
    tracker: RefCell<PlayTracker>,
    sending: Cell<bool>,
    backoff: Cell<Option<Duration>>,
    retry: RefCell<Option<glib::SourceId>>,
}

pub fn setup(app: &LioApplication, controller: &PlaybackController) {
    let state = Rc::new(State::default());

    controller.connect_track_started(glib::clone!(
        #[weak]
        app,
        #[strong]
        state,
        move |c| {
            let Some(track) = c.current_track() else {
                return;
            };
            state
                .tracker
                .borrow_mut()
                .start(track.id(), track.duration_s(), lio_store::db::now());
            if !app.settings().scrobble() {
                return;
            }
            // Now playing: best effort, never queued.
            let session = app.session();
            if let (Some(account), true) = (session.account(), session.is_online()) {
                let id = track.id().to_owned();
                runtime::spawn(async move {
                    let _ = account.client.scrobble(&id, false, None).await;
                });
            }
        }
    ));

    controller.connect_position_ms_notify(glib::clone!(
        #[weak]
        app,
        #[strong]
        state,
        move |c| {
            let done = state
                .tracker
                .borrow_mut()
                .tick(c.position_ms(), c.playing());
            let Some(play) = done else { return };
            if !app.settings().scrobble() {
                return;
            }
            let Some(account) = app.session().account() else {
                return;
            };
            if let Err(e) = account.db.with(|conn| scrobble::enqueue(conn, &play)) {
                tracing::warn!("could not queue a play: {e}");
                return;
            }
            tracing::debug!(track = %play.track_id, "play recorded");
            submit(&app, &state);
        }
    ));

    controller.connect_playing_notify(glib::clone!(
        #[strong]
        state,
        move |c| {
            if !c.playing() {
                state.tracker.borrow_mut().pause();
            }
        }
    ));

    // Deliver plays made offline as soon as the server is back, and on launch.
    app.session().connect_state_notify(glib::clone!(
        #[weak]
        app,
        #[strong]
        state,
        move |session| {
            if session.state() == SessionState::SignedIn {
                state.backoff.set(None);
                submit(&app, &state);
            }
        }
    ));
}

fn submit(app: &LioApplication, state: &Rc<State>) {
    if state.sending.replace(true) {
        return;
    }
    if let Some(id) = state.retry.take() {
        id.remove();
    }
    let session = app.session();
    let (Some(account), true) = (session.account(), session.is_online()) else {
        state.sending.set(false);
        return;
    };
    let app = app.clone();
    let state = state.clone();
    glib::spawn_future_local(async move {
        let (client, db) = (account.client.clone(), account.db.clone());
        let result =
            runtime::run(async move { scrobble::submit_pending(&client, &db).await }).await;
        state.sending.set(false);
        match result {
            Ok(n) => {
                state.backoff.set(None);
                if n > 0 {
                    tracing::info!(plays = n, "scrobbles delivered");
                }
            }
            Err(e) => {
                let delay = state
                    .backoff
                    .get()
                    .map_or(MIN_BACKOFF, |d| (d * 2).min(MAX_BACKOFF));
                state.backoff.set(Some(delay));
                tracing::info!(
                    retry_in_s = delay.as_secs(),
                    "scrobble submission failed: {e}"
                );
                let id = glib::timeout_add_local_once(
                    delay,
                    glib::clone!(
                        #[weak]
                        app,
                        #[strong]
                        state,
                        move || {
                            state.retry.take();
                            submit(&app, &state);
                        }
                    ),
                );
                state.retry.replace(Some(id));
            }
        }
    });
}
