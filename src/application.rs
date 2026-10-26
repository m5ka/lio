//! The `LioApplication`: lifetime, actions, and wiring of session, playback and window.

use std::cell::{OnceCell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, gio, glib};

use crate::config;
use crate::covers::Covers;
use crate::library_sync::LioLibrary;
use crate::playback_controller::PlaybackController;
use crate::session::{LioSession, SessionState};
use crate::settings::Settings;
use crate::widgets::{
    LioAlbumGrid, LioFavouritesPage, LioHomePage, LioPlayerBar, LioSignInPage, LioWindow, Section,
    pages::library_pages,
};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LioApplication {
        pub settings: OnceCell<Settings>,
        pub session: OnceCell<LioSession>,
        pub playback: OnceCell<PlaybackController>,
        pub library: OnceCell<LioLibrary>,
        pub offline: OnceCell<crate::offline::LioOffline>,
        pub covers: RefCell<Option<Covers>>,
        pub window: RefCell<glib::WeakRef<LioWindow>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioApplication {
        const NAME: &'static str = "LioApplication";
        type Type = super::LioApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for LioApplication {}

    impl ApplicationImpl for LioApplication {
        fn startup(&self) {
            self.parent_startup();
            let obj = self.obj();
            gtk::Window::set_default_icon_name(config::APP_ID);
            if let Err(e) = lio_playback::init() {
                tracing::error!("GStreamer initialisation failed: {e}");
            }
            let settings = obj.settings().clone();
            let session = LioSession::new(&settings);
            let playback = PlaybackController::new(&settings);
            self.library
                .set(LioLibrary::new(&session))
                .expect("startup runs once");
            self.session.set(session).expect("startup runs once");
            self.playback
                .set(playback.clone())
                .expect("startup runs once");
            self.offline
                .set(crate::offline::LioOffline::new(&obj))
                .expect("startup runs once");
            crate::desktop::background::setup(&obj, &playback);
            crate::desktop::mpris::setup(&obj, &playback);
            crate::desktop::tray::setup(&obj, &playback);
            crate::desktop::notifications::setup(&obj, &playback);
            crate::scrobbler::setup(&obj, &playback);
            obj.setup_actions();
            obj.setup_session();
        }

        fn activate(&self) {
            self.parent_activate();
            self.obj().present_main_window();
        }

        fn shutdown(&self) {
            if let Some(playback) = self.playback.get() {
                playback.save_now();
                playback.stop();
            }
            self.parent_shutdown();
        }
    }

    impl GtkApplicationImpl for LioApplication {}
    impl AdwApplicationImpl for LioApplication {}
}

glib::wrapper! {
    pub struct LioApplication(ObjectSubclass<imp::LioApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl LioApplication {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", config::APP_ID)
            .property("flags", gio::ApplicationFlags::empty())
            .property("resource-base-path", config::RESOURCE_BASE)
            .build()
    }

    pub fn settings(&self) -> &Settings {
        self.imp().settings.get_or_init(Settings::new)
    }

    pub fn session(&self) -> LioSession {
        self.imp()
            .session
            .get()
            .expect("session exists after startup")
            .clone()
    }

    pub fn playback(&self) -> PlaybackController {
        self.imp()
            .playback
            .get()
            .expect("playback exists after startup")
            .clone()
    }

    pub fn library(&self) -> LioLibrary {
        self.imp()
            .library
            .get()
            .expect("library exists after startup")
            .clone()
    }

    pub fn offline(&self) -> crate::offline::LioOffline {
        self.imp()
            .offline
            .get()
            .expect("offline exists after startup")
            .clone()
    }

    pub fn covers(&self) -> Option<Covers> {
        self.imp().covers.borrow().clone()
    }

    pub fn window(&self) -> Option<LioWindow> {
        self.imp().window.borrow().upgrade()
    }

    /// The main window, created on first use.
    pub fn main_window(&self) -> LioWindow {
        if let Some(win) = self.window() {
            return win;
        }
        let win = LioWindow::new(self);
        crate::dev::debug::apply_test_size(&win);
        win.set_section_builder(build_section);
        win.set_player_bar(&LioPlayerBar::new(&self.playback()));
        win.set_queue_sidebar(&crate::widgets::LioQueueSidebar::new(&self.playback()));
        self.setup_window_keys(&win);
        self.setup_window_actions(&win);
        crate::dev::frame_stats::setup(&win);
        crate::actions::items::install(&win);
        self.library().connect_syncing_notify(glib::clone!(
            #[weak]
            win,
            move |lib| win.set_syncing(lib.syncing())
        ));
        // Playlists are sidebar entries: follow library changes while the window lives.
        let changed = self.library().connect_changed(glib::clone!(
            #[weak]
            win,
            move |_| win.refresh_playlists()
        ));
        let library = self.library().clone();
        let changed = std::cell::Cell::new(Some(changed));
        win.connect_destroy(move |_| {
            if let Some(id) = changed.take() {
                library.disconnect(id);
            }
        });
        self.imp().window.replace(win.downgrade());
        win
    }

    fn present_main_window(&self) {
        self.present_main_window_with_token(None);
    }

    /// Like activating the app; `token` is an XDG activation token (from the tray
    /// icon), so Wayland compositors give the window focus.
    pub fn present_main_window_with_token(&self, token: Option<&str>) {
        let first = self.window().is_none();
        let win = self.main_window();
        if let Some(token) = token {
            win.set_startup_id(token);
        }
        win.present();
        if first && self.session().account().is_some() {
            // Reopened while playing in the background: same session, new window.
            self.on_session_state(self.session().state());
            win.set_player_bar_visible(self.playback().current_track().is_some());
        } else if first {
            win.show_loading();
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = app)]
                self,
                async move {
                    if !app.session().restore().await {
                        app.show_sign_in(None);
                    }
                }
            ));
            crate::dev::debug::schedule_screenshot(&win);
        }
    }

    /// Whether closing the window should only hide it (background playback).
    pub fn should_hide_on_close(&self) -> bool {
        self.settings().close_to_background() && self.playback().playing()
    }

    // Session ---------------------------------------------------------------------------

    fn setup_session(&self) {
        let session = self.session();
        session.connect_state_notify(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |session| app.on_session_state(session.state())
        ));
        self.playback().connect_current_track_notify(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |c| {
                if let Some(win) = app.window() {
                    win.set_player_bar_visible(c.current_track().is_some());
                }
            }
        ));
        self.playback().connect_error(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, message| {
                if let Some(win) = app.window() {
                    win.toast(message);
                }
            }
        ));
    }

    fn on_session_state(&self, state: SessionState) {
        let Some(win) = self.window() else { return };
        let banner = win.banner();
        match state {
            SessionState::SignedIn | SessionState::Offline => {
                self.attach_account();
                let offline = state == SessionState::Offline;
                banner.set_title(&gettext(
                    "You're offline — showing downloaded and cached music",
                ));
                let go_online = gettext("Go Online");
                banner
                    .set_button_label(self.settings().offline_mode().then_some(go_online.as_str()));
                banner.set_revealed(offline);
                win.show_library();
                // Pages re-query so unavailable tracks are shown as such.
                self.library().emit_changed();
            }
            SessionState::ReauthRequired => {
                let settings = self.settings();
                self.show_sign_in(Some((settings.server_url(), settings.username())));
                if let Some(page) = win.sign_in_page() {
                    page.show_error(&gettext(
                        "Your session has expired. Sign in again to continue.",
                    ));
                }
            }
            SessionState::SignedOut => {
                self.detach_account();
                win.reset_library();
                self.show_sign_in(None);
            }
            SessionState::SigningIn => {}
        }
    }

    fn attach_account(&self) {
        let Some(account) = self.session().account() else {
            return;
        };
        let covers = self.imp().covers.borrow().clone();
        match covers {
            Some(covers) => covers.set_client(Some(account.client.clone())),
            None => {
                self.imp().covers.replace(Some(Covers::new(
                    Some(account.client.clone()),
                    account.paths.covers(),
                )));
            }
        }
        self.playback().set_store(Some(account.tracks.clone()));
        if let Some(win) = self.window() {
            win.set_player_bar_visible(self.playback().current_track().is_some());
        }
        self.offline().attach(self);
        if let Some(win) = self.window() {
            crate::actions::items::set_enabled(&win, "download", account.can_download);
        }
        if self.session().is_online() {
            self.library().start();
        }
        crate::dev::debug::maybe_autoplay(self);
        crate::dev::debug::maybe_show_section(self);
        crate::dev::debug::maybe_activate_action(self);
        crate::dev::debug::maybe_download(self);
    }

    fn detach_account(&self) {
        self.offline().detach();
        self.library().stop();
        self.playback().set_store(None);
        self.imp().covers.replace(None);
        if let Some(win) = self.window() {
            win.set_player_bar_visible(false);
        }
    }

    fn show_sign_in(&self, prefill: Option<(String, String)>) {
        let Some(win) = self.window() else { return };
        let page = LioSignInPage::new();
        if let Some((server, user)) = prefill {
            page.prefill(&server, &user);
        }
        page.connect_signed_in(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, limited| {
                if limited && let Some(win) = app.window() {
                    win.toast(&gettext("This server may not be fully compatible with Lio"));
                }
            }
        ));
        win.show_sign_in(&page);
        page.grab_focus();
    }

    async fn confirm_sign_out(&self) {
        let Some(win) = self.window() else { return };
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Sign Out?"))
            .body(gettext(
                "Downloads and cached music will be deleted from this device.",
            ))
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("sign-out", &gettext("_Sign Out")),
        ]);
        dialog.set_response_appearance("sign-out", adw::ResponseAppearance::Destructive);
        if dialog.choose_future(Some(&win)).await == "sign-out" {
            self.playback().stop();
            self.session().sign_out().await;
        }
    }

    // Actions ---------------------------------------------------------------------------

    fn setup_actions(&self) {
        let action = |name: &str, f: fn(&Self)| {
            gio::ActionEntry::builder(name)
                .activate(move |app: &Self, _, _| f(app))
                .build()
        };
        self.add_action_entries([
            action("quit", |app| {
                app.playback().save_now();
                app.quit();
            }),
            action("about", |app| app.show_about()),
            action("preferences", |app| {
                if let Some(win) = app.window() {
                    crate::widgets::preferences::LioPreferences::new().present(Some(&win));
                }
            }),
            action("play-pause", |app| app.playback().play_pause()),
            action("next", |app| app.playback().next()),
            action("previous", |app| app.playback().previous()),
            action("seek-forward", |app| app.playback().seek_relative(true)),
            action("seek-backward", |app| app.playback().seek_relative(false)),
            action("volume-up", |app| app.playback().change_volume(0.05)),
            action("volume-down", |app| app.playback().change_volume(-0.05)),
            action("sign-out", |app| {
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    app,
                    async move { app.confirm_sign_out().await }
                ));
            }),
        ]);

        // Keys that clash with text entry or list navigation are handled by the window's
        // key controller instead (see `setup_window_keys`).
        self.set_accels_for_action("app.quit", &["<Control>q"]);
        self.set_accels_for_action("app.preferences", &["<Control>comma"]);
        self.set_accels_for_action(
            "app.play-pause",
            &["<Control>space", "AudioPlay", "AudioPause"],
        );
        self.set_accels_for_action("app.next", &["AudioNext"]);
        self.set_accels_for_action("app.previous", &["AudioPrev"]);
        self.set_accels_for_action("app.volume-up", &["<Control>Up"]);
        self.set_accels_for_action("app.volume-down", &["<Control>Down"]);

        // Shuffle (bool) and repeat (cycles off/all/one) mirror the controller's properties.
        let playback = self.playback();
        let shuffle = gio::PropertyAction::new("shuffle", &playback, "shuffle");
        self.add_action(&shuffle);
        self.set_accels_for_action("app.shuffle", &["<Control>s"]);
        let repeat = gio::ActionEntry::builder("repeat")
            .activate(|app: &Self, _, _| app.playback().cycle_repeat())
            .build();
        self.add_action_entries([repeat]);
        self.set_accels_for_action("app.repeat", &["<Control>r"]);

        // Manual offline mode, bound to the `offline-mode` setting.
        self.add_action(&self.settings().create_action("offline-mode"));

        let refresh = gio::ActionEntry::builder("refresh-library")
            .activate(|app: &Self, _, _| {
                app.session().check_connection();
                app.library().sync(true);
            })
            .build();
        self.add_action_entries([refresh]);
        self.set_accels_for_action("app.refresh-library", &["F5", "<Control>F5"]);

        let playback = self.playback();
        let enable = glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |c: &PlaybackController| {
                for (name, enabled) in [
                    ("next", c.has_next()),
                    ("previous", c.has_previous()),
                    ("seek-forward", c.active()),
                    ("seek-backward", c.active()),
                ] {
                    if let Some(a) = app.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                        a.set_enabled(enabled);
                    }
                }
            }
        );
        enable(&playback);
        for prop in ["has-next", "has-previous", "active"] {
            let enable = enable.clone();
            playback.connect_notify_local(Some(prop), move |c, _| enable(c));
        }
    }

    /// `win.search` and `win.go-*`.
    fn setup_window_actions(&self, win: &LioWindow) {
        let search = gio::ActionEntry::builder("search")
            .activate(|win: &LioWindow, _, _| win.show_search())
            .build();
        win.add_action_entries([search]);
        self.set_accels_for_action("win.search", &["<Control>f"]);

        let shortcuts = gio::ActionEntry::builder("show-help-overlay")
            .activate(|win: &LioWindow, _, _| {
                let builder = gtk::Builder::from_resource("/dev/m5ka/Lio/ui/shortcuts.ui");
                if let Some(dialog) = builder.object::<adw::ShortcutsDialog>("shortcuts_dialog") {
                    dialog.present(Some(win));
                }
            })
            .build();
        win.add_action_entries([shortcuts]);
        self.set_accels_for_action("win.show-help-overlay", &["<Control>question"]);

        let now_playing = gio::ActionEntry::builder("now-playing")
            .activate(|win: &LioWindow, _, _| {
                if win.is_now_playing_shown() {
                    win.hide_now_playing();
                } else {
                    win.show_now_playing();
                }
            })
            .build();
        win.add_action_entries([now_playing]);
        self.set_accels_for_action("win.now-playing", &["<Control>Return"]);

        // Stateful toggle bound to the queue sidebar.
        let show_queue = gio::PropertyAction::new("show-queue", &win.queue_split(), "show-sidebar");
        win.add_action(&show_queue);
        self.set_accels_for_action("win.show-queue", &["<Control>u"]);
        for (i, section) in Section::ALL.into_iter().enumerate() {
            let name = format!("go-{}", section.name());
            let entry = gio::ActionEntry::builder(&name)
                .activate(move |win: &LioWindow, _, _| win.show_section(section))
                .build();
            win.add_action_entries([entry]);
            self.set_accels_for_action(&format!("win.{name}"), &[&format!("<Alt>{}", i + 1)]);
        }
    }

    /// Space, Ctrl+←/→ and Shift+←/→ only act when they wouldn't steal input from the
    /// focused widget.
    fn setup_window_keys(&self, win: &LioWindow) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            win,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let mods = state & gtk::accelerator_get_default_mod_mask();
                let focus = gtk::prelude::GtkWindowExt::focus(&win);
                let editing = focus.as_ref().is_some_and(|w| w.is::<gtk::Editable>());
                let action = match (key, mods) {
                    (gdk::Key::space, m) if m.is_empty() && !space_is_taken(focus.as_ref()) => {
                        "play-pause"
                    }
                    (gdk::Key::Right, gdk::ModifierType::CONTROL_MASK) if !editing => "next",
                    (gdk::Key::Left, gdk::ModifierType::CONTROL_MASK) if !editing => "previous",
                    (gdk::Key::Right, gdk::ModifierType::SHIFT_MASK) if !editing => "seek-forward",
                    (gdk::Key::Left, gdk::ModifierType::SHIFT_MASK) if !editing => "seek-backward",
                    _ => return glib::Propagation::Proceed,
                };
                if win.is_visible() && app.session().account().is_some() {
                    app.activate_action(action, None);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        ));
        win.add_controller(keys);
    }

    fn show_about(&self) {
        let dialog = adw::AboutDialog::builder()
            .application_name("Lio")
            // The logo with the name (bundled; see build-aux/make-icons.sh).
            .application_icon("lio-branding")
            .developer_name("m5ka")
            .developers(["m5ka <git@m5ka.dev>"])
            .version(config::VERSION)
            .website("https://github.com/m5ka/lio")
            .issue_url("https://github.com/m5ka/lio/issues")
            .license_type(gtk::License::Gpl30)
            .comments(gettext("Listen to music from your Navidrome server"))
            .build();
        dialog.present(self.window().as_ref());
    }
}

/// Whether the focused widget uses Space itself (text, buttons, list rows/items).
fn space_is_taken(focus: Option<&gtk::Widget>) -> bool {
    let Some(w) = focus else { return false };
    w.is::<gtk::Editable>()
        || w.is::<gtk::Button>()
        || w.is::<gtk::CheckButton>()
        || w.is::<gtk::Switch>()
        || w.is::<gtk::ListBoxRow>()
        || w.is::<gtk::Scale>()
        || w.ancestor(gtk::ListView::static_type()).is_some()
        || w.ancestor(gtk::GridView::static_type()).is_some()
}

fn build_section(section: Section) -> adw::NavigationPage {
    match section {
        Section::Home => LioHomePage::new().upcast(),
        Section::Albums => LioAlbumGrid::albums().upcast(),
        Section::Artists => library_pages::artists().upcast(),
        Section::Genres => library_pages::genres().upcast(),
        Section::Favourites => LioFavouritesPage::new().upcast(),
        Section::Downloads => crate::widgets::pages::downloads_page::page(),
    }
}

impl Default for LioApplication {
    fn default() -> Self {
        gio::Application::default()
            .and_downcast::<LioApplication>()
            .expect("the default application is a LioApplication")
    }
}
