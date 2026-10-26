//! Seek bars that scrub without seeking. While the knob is held, only the knob and the
//! position label move; the seek happens once, on release. Seeking all the way through
//! a drag stutters, and a drag across the end of the bar would finish the track.
//! Keyboard and scroll changes are debounced instead, so holding a key seeks once.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};

/// How long keyboard and scroll changes settle before seeking.
const DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Default)]
struct State {
    /// A pointer or touch is down on the scale.
    held: Cell<bool>,
    /// Where the user moved the knob, in seconds, until it is seeked to.
    target: Cell<Option<f64>>,
    debounce: RefCell<Option<glib::SourceId>>,
}

pub struct SeekScale {
    state: Rc<State>,
}

impl SeekScale {
    /// Handles user changes to `scale`: `preview(seconds)` while the knob moves,
    /// `commit(seconds)` to seek. The caller stops seeking on the adjustment's
    /// `value-changed` and leaves the scale alone while [`is_scrubbing`](Self::is_scrubbing).
    pub fn attach(
        scale: &gtk::Scale,
        preview: impl Fn(f64) + 'static,
        commit: impl Fn(f64) + 'static,
    ) -> Self {
        let state = Rc::new(State::default());
        let commit: Rc<dyn Fn(f64)> = Rc::new(commit);

        // Capture phase, and a legacy controller rather than a gesture: the range's own
        // drag gesture claims the press, and a gesture would only see it cancelled.
        let events = gtk::EventControllerLegacy::new();
        events.set_propagation_phase(gtk::PropagationPhase::Capture);
        events.connect_event(glib::clone!(
            #[strong]
            state,
            #[strong]
            commit,
            move |_, event| {
                match event.event_type() {
                    gdk::EventType::ButtonPress | gdk::EventType::TouchBegin => {
                        state.held.set(true);
                    }
                    gdk::EventType::ButtonRelease
                    | gdk::EventType::TouchEnd
                    | gdk::EventType::TouchCancel => {
                        state.held.set(false);
                        if let Some(target) = state.target.take() {
                            commit(target);
                        }
                    }
                    _ => {}
                }
                glib::Propagation::Proceed
            }
        ));
        scale.add_controller(events);

        // Only user changes come through here, not the position updates.
        scale.connect_change_value(glib::clone!(
            #[strong]
            state,
            move |scale, _, value| {
                let adj = scale.adjustment();
                let value = value.clamp(adj.lower(), adj.upper());
                preview(value);
                state.target.set(Some(value));
                if let Some(source) = state.debounce.take() {
                    source.remove();
                }
                if !state.held.get() {
                    let source = glib::timeout_add_local_once(
                        DEBOUNCE,
                        glib::clone!(
                            #[strong]
                            state,
                            #[strong]
                            commit,
                            move || {
                                state.debounce.take();
                                if let Some(target) = state.target.take() {
                                    commit(target);
                                }
                            }
                        ),
                    );
                    state.debounce.replace(Some(source));
                }
                glib::Propagation::Proceed
            }
        ));
        Self { state }
    }

    /// The user is moving the knob, or a seek to where they left it is still to come.
    pub fn is_scrubbing(&self) -> bool {
        self.state.held.get() || self.state.target.get().is_some()
    }
}
