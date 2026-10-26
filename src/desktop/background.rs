//! Background playback: keep the app alive and the system awake while
//! music plays; ask the Background portal for permission when sandboxed.

use std::cell::RefCell;

use gettextrs::gettext;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::application::LioApplication;
use crate::playback_controller::PlaybackController;
use crate::runtime;

#[derive(Default)]
struct State {
    hold: Option<gio::ApplicationHoldGuard>,
    inhibit_cookie: Option<u32>,
    portal_requested: bool,
}

/// Wires hold/inhibit to the controller's `playing` property.
pub fn setup(app: &LioApplication, controller: &PlaybackController) {
    let state = std::rc::Rc::new(RefCell::new(State::default()));
    controller.connect_playing_notify(glib::clone!(
        #[weak]
        app,
        move |controller| {
            let mut st = state.borrow_mut();
            if controller.playing() {
                if st.hold.is_none() {
                    st.hold = Some(app.hold());
                }
                if st.inhibit_cookie.is_none() {
                    let window = app.window();
                    let cookie = app.inhibit(
                        window.as_ref(),
                        gtk::ApplicationInhibitFlags::SUSPEND,
                        Some(&gettext("Playing music")),
                    );
                    st.inhibit_cookie = (cookie != 0).then_some(cookie);
                }
                if !st.portal_requested && ashpd::is_sandboxed() {
                    st.portal_requested = true;
                    request_background_portal();
                }
            } else {
                st.hold = None;
                if let Some(cookie) = st.inhibit_cookie.take() {
                    app.uninhibit(cookie);
                }
            }
        }
    ));
}

fn request_background_portal() {
    glib::spawn_future_local(async {
        let reason = gettext("Keep playing music when the window is closed");
        let result = runtime::run(async move {
            ashpd::desktop::background::Background::request()
                .reason(reason.as_str())
                .auto_start(false)
                .dbus_activatable(false)
                .send()
                .await?
                .response()
        })
        .await;
        match result {
            Ok(response) if response.run_in_background() => {
                tracing::debug!("background playback allowed")
            }
            Ok(_) => tracing::info!("background playback denied by the user"),
            Err(e) => tracing::warn!("background portal unavailable: {e}"),
        }
    });
}
