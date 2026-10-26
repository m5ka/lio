//! `LioLibrary`: runs library syncs for the signed-in account and tells pages to reload.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use gtk::glib;
use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use lio_store::sync;

use crate::runtime;
use crate::session::{Account, LioSession};

/// Track lists fetched per background step.
const TRACK_FILL_BATCH: usize = 20;
/// How often a quick incremental sync runs while the app is open.
const RESYNC_INTERVAL: Duration = Duration::from_secs(15 * 60);

mod imp {
    use std::sync::LazyLock;

    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::LioLibrary)]
    pub struct LioLibrary {
        pub session: RefCell<Option<LioSession>>,
        #[property(get)]
        pub syncing: Cell<bool>,
        pub generation: Cell<u64>,
        pub timer: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioLibrary {
        const NAME: &'static str = "LioLibrary";
        type Type = super::LioLibrary;
    }

    #[glib::derived_properties]
    impl ObjectImpl for LioLibrary {
        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                // The mirror changed; pages should re-query.
                vec![Signal::builder("changed").build()]
            });
            SIGNALS.as_ref()
        }
    }
}

glib::wrapper! {
    pub struct LioLibrary(ObjectSubclass<imp::LioLibrary>);
}

impl LioLibrary {
    pub fn new(session: &LioSession) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().session.replace(Some(session.clone()));
        obj
    }

    fn account(&self) -> Option<Account> {
        self.imp()
            .session
            .borrow()
            .as_ref()
            .and_then(|s| s.account())
    }

    fn online(&self) -> bool {
        self.imp()
            .session
            .borrow()
            .as_ref()
            .is_some_and(|s| s.is_online())
    }

    pub fn connect_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "changed",
            false,
            glib::closure_local!(move |obj: &Self| f(obj)),
        )
    }

    pub fn emit_changed(&self) {
        self.emit_by_name::<()>("changed", &[]);
    }

    /// Starts syncing for a newly attached account and schedules periodic re-syncs.
    pub fn start(&self) {
        self.sync(false);
        if self.imp().timer.borrow().is_none() {
            let id = glib::timeout_add_local(
                RESYNC_INTERVAL,
                glib::clone!(
                    #[weak(rename_to = lib)]
                    self,
                    #[upgrade_or]
                    glib::ControlFlow::Break,
                    move || {
                        lib.sync(false);
                        glib::ControlFlow::Continue
                    }
                ),
            );
            self.imp().timer.replace(Some(id));
        }
    }

    /// Stops background work (sign-out).
    pub fn stop(&self) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get() + 1);
        if let Some(id) = imp.timer.take() {
            id.remove();
        }
        if imp.syncing.replace(false) {
            self.notify_syncing();
        }
    }

    /// Runs a library sync (`force_full` relists every album), then fills missing
    /// track lists in the background. Does nothing offline or while a sync runs.
    pub fn sync(&self, force_full: bool) {
        let imp = self.imp();
        let (Some(account), true) = (self.account(), self.online()) else {
            return;
        };
        if imp.syncing.replace(true) {
            return;
        }
        self.notify_syncing();
        let generation = imp.generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = lib)]
            self,
            async move {
                let (client, db, user) = (
                    account.client.clone(),
                    account.db.clone(),
                    account.username.clone(),
                );
                let result =
                    runtime::run(async move { sync::sync(&client, &db, &user, force_full).await })
                        .await;
                if lib.imp().generation.get() != generation {
                    return;
                }
                match result {
                    Ok(report) => {
                        if !report.unchanged || report.playlists > 0 {
                            lib.emit_changed();
                        }
                    }
                    Err(e) => tracing::warn!("library sync failed: {e}"),
                }
                lib.fill_tracks(account, generation).await;
                if lib.imp().generation.get() == generation && lib.imp().syncing.replace(false) {
                    lib.notify_syncing();
                }
            }
        ));
    }

    /// Fetches album track lists in batches until none are missing, so offline search and
    /// downloads cover every track.
    async fn fill_tracks(&self, account: Account, generation: u64) {
        let mut total = 0;
        loop {
            if self.imp().generation.get() != generation || !self.online() {
                return;
            }
            let (client, db) = (account.client.clone(), account.db.clone());
            match runtime::run(
                async move { sync::sync_tracks(&client, &db, TRACK_FILL_BATCH).await },
            )
            .await
            {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(e) => {
                    tracing::debug!("track fill paused: {e}");
                    break;
                }
            }
        }
        if total > 0 {
            tracing::info!(albums = total, "fetched missing track lists");
            self.emit_changed();
        }
    }
}
