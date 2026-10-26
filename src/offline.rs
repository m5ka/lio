//! Offline listening: downloads worker, network policy, cache limit and offline
//! mode.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use gettextrs::{gettext, ngettext};
use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};
use lio_store::Quality;
use lio_store::downloads::{Downloader, Event, PinKind, Policy};

use crate::application::LioApplication;
use crate::runtime;
use crate::session::SessionState;

/// How often an automatically offline session checks whether the server is back.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

mod imp {
    use std::sync::LazyLock;

    use super::*;

    #[derive(Default)]
    pub struct LioOffline {
        pub downloader: RefCell<Option<Downloader>>,
        pub reconnect: RefCell<Option<glib::SourceId>>,
        pub sync_pending: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioOffline {
        const NAME: &'static str = "LioOffline";
        type Type = super::LioOffline;
    }

    impl ObjectImpl for LioOffline {
        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                // A download's status or progress changed (0 = several).
                vec![
                    Signal::builder("downloads-changed")
                        .param_types([i64::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }
}

glib::wrapper! {
    pub struct LioOffline(ObjectSubclass<imp::LioOffline>);
}

impl LioOffline {
    pub fn new(app: &LioApplication) -> Self {
        let obj: Self = glib::Object::new();
        obj.setup(app);
        obj
    }

    pub fn downloader(&self) -> Option<Downloader> {
        self.imp().downloader.borrow().clone()
    }

    pub fn connect_downloads_changed<F: Fn(&Self, i64) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "downloads-changed",
            false,
            glib::closure_local!(move |obj: &Self, pin: i64| f(obj, pin)),
        )
    }

    fn setup(&self, app: &LioApplication) {
        let settings = app.settings();
        let monitor = gio::NetworkMonitor::default();
        // Automatic offline detection.
        monitor.connect_network_changed(glib::clone!(
            #[weak]
            app,
            move |_, available| {
                let session = app.session();
                if app.settings().offline_mode() || session.account().is_none() {
                    return;
                }
                if available {
                    session.check_connection();
                } else {
                    session.set_state(SessionState::Offline);
                }
            }
        ));
        monitor.connect_network_metered_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            app,
            move |_| this.update_policy(&app)
        ));
        settings.connect_changed_key(
            "download-on-metered",
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                app,
                move |_| this.update_policy(&app)
            ),
        );
        settings.connect_changed_key(
            "cache-limit-mb",
            glib::clone!(
                #[weak]
                app,
                move |s| {
                    if let Some(account) = app.session().account() {
                        account.tracks.set_cache_limit(s.cache_limit_bytes());
                        let store = account.tracks.clone();
                        runtime::spawn(async move {
                            let _ =
                                tokio::task::spawn_blocking(move || store.enforce_cache_limit())
                                    .await;
                        });
                    }
                }
            ),
        );
        // Manual offline mode.
        settings.connect_changed_key(
            "offline-mode",
            glib::clone!(
                #[weak]
                app,
                move |s| {
                    let session = app.session();
                    if session.account().is_none() {
                        return;
                    }
                    if s.offline_mode() {
                        session.set_state(SessionState::Offline);
                    } else {
                        session.check_connection();
                    }
                }
            ),
        );
        app.session().connect_state_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            app,
            move |session| {
                this.update_policy(&app);
                this.schedule_reconnect(&app, session.state() == SessionState::Offline);
            }
        ));
        // Keep downloaded playlists and Favourites in step with the server.
        app.library().connect_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.sync_pins()
        ));
    }

    /// Starts the downloads worker for the signed-in account.
    pub fn attach(&self, app: &LioApplication) {
        let Some(account) = app.session().account() else {
            return;
        };
        if self.imp().downloader.borrow().is_some() {
            if let Some(dl) = self.downloader() {
                dl.set_client(Some(account.client.clone()));
            }
            self.update_policy(app);
            return;
        }
        account
            .tracks
            .set_cache_limit(app.settings().cache_limit_bytes());
        let (downloader, events) = Downloader::new(account.db.clone(), account.paths.clone());
        downloader.set_client(Some(account.client.clone()));
        runtime::spawn(downloader.clone().run());
        self.imp().downloader.replace(Some(downloader));
        self.update_policy(app);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                while let Ok(event) = events.recv().await {
                    this.handle_event(event);
                }
            }
        ));
    }

    pub fn detach(&self) {
        if let Some(dl) = self.imp().downloader.take() {
            dl.stop();
        }
        if let Some(id) = self.imp().reconnect.take() {
            id.remove();
        }
    }

    fn handle_event(&self, event: Event) {
        let win = LioApplication::default().window();
        match event {
            Event::Changed(pin) => self.emit_by_name::<()>("downloads-changed", &[&pin]),
            Event::TracksRemoved { count, .. } => {
                if let Some(win) = win {
                    win.toast(
                        &ngettext(
                            "A downloaded song was removed from the server",
                            "{} downloaded songs were removed from the server",
                            count as u32,
                        )
                        .replace("{}", &count.to_string()),
                    );
                }
                self.emit_by_name::<()>("downloads-changed", &[&0i64]);
            }
            Event::DiskFull => {
                if let Some(win) = win {
                    win.toast(&gettext("Downloads paused: not enough disk space"));
                }
            }
        }
    }

    fn update_policy(&self, app: &LioApplication) {
        let Some(dl) = self.downloader() else { return };
        let monitor = gio::NetworkMonitor::default();
        dl.set_policy(Policy {
            online: app.session().is_online(),
            metered: monitor.is_network_metered(),
            allow_metered: app.settings().download_on_metered(),
        });
    }

    /// While automatically offline, re-checks the server every 30 s.
    fn schedule_reconnect(&self, app: &LioApplication, offline: bool) {
        let imp = self.imp();
        if let Some(id) = imp.reconnect.take() {
            id.remove();
        }
        if !offline || app.settings().offline_mode() {
            return;
        }
        let id = glib::timeout_add_local(
            RECONNECT_INTERVAL,
            glib::clone!(
                #[weak]
                app,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    app.session().check_connection();
                    glib::ControlFlow::Continue
                }
            ),
        );
        imp.reconnect.replace(Some(id));
    }

    fn sync_pins(&self) {
        let imp = self.imp();
        let Some(dl) = self.downloader() else { return };
        if imp.sync_pending.replace(true) {
            return;
        }
        glib::timeout_add_local_once(
            Duration::from_millis(500),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.imp().sync_pending.set(false);
                    runtime::spawn(async move {
                        if let Err(e) = dl.sync_pins().await {
                            tracing::warn!("download sync failed: {e}");
                        }
                    });
                }
            ),
        );
    }

    /// Pins or unpins `(kind, id)` for offline listening.
    pub async fn toggle(&self, kind: PinKind, id: &str) -> Result<bool, String> {
        let Some(dl) = self.downloader() else {
            return Err(gettext("Downloads aren't available"));
        };
        let Some(account) = LioApplication::default().session().account() else {
            return Ok(false);
        };
        let existing = account
            .db
            .read(|c| lio_store::downloads::find_pin(c, kind, id))
            .map_err(|e| e.to_string())?;
        let quality: Quality = LioApplication::default().settings().download_quality();
        let id = id.to_owned();
        let result = runtime::run(async move {
            match existing {
                Some(pin) => dl.remove(pin.rowid).await.map(|()| false),
                None => {
                    // Album track lists may not be mirrored yet.
                    if kind == PinKind::Album {
                        let synced = account
                            .db
                            .run({
                                let id = id.clone();
                                move |c| {
                                    Ok(lio_store::queries::album_row_by_id(c, &id)?
                                        .and_then(|a| a.tracks_synced_at))
                                }
                            })
                            .await?;
                        if synced.is_none() {
                            lio_store::sync::sync_album_tracks(&account.client, &account.db, &id)
                                .await?;
                        }
                    }
                    dl.add(kind, &id, quality).await.map(|_| true)
                }
            }
        })
        .await;
        result.map_err(|e| e.to_string())
    }

    /// Whether `(kind, id)` is pinned, and complete.
    pub fn pin_state(&self, kind: PinKind, id: &str) -> Option<lio_store::downloads::Pin> {
        let account = LioApplication::default().session().account()?;
        account
            .db
            .read(|c| lio_store::downloads::find_pin(c, kind, id))
            .ok()
            .flatten()
    }
}
