//! Status notifier ("tray") icon: lets a closed
//! window be brought back while Lio keeps playing, with basic playback controls.
//!
//! Uses the StatusNotifierItem D-Bus protocol (KDE Plasma, most other desktops; GNOME
//! with the AppIndicator extension). While the icon is shown and a track is loaded, Lio
//! keeps running even when paused with no window, so it can be resumed from the icon;
//! without a tray host it quits as before once nothing plays.

use std::cell::RefCell;
use std::rc::Rc;

use gettextrs::gettext;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, gsk};
use ksni::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};

use crate::application::LioApplication;
use crate::config;
use crate::playback_controller::PlaybackController;
use crate::runtime;

/// Requests from the tray (its own thread) to the GTK main thread.
enum Command {
    /// Show the window; carries the XDG activation token for Wayland focus.
    Show(Option<String>),
    PlayPause,
    Next,
    Previous,
    Quit,
    /// The tray host (StatusNotifierWatcher) appeared or went away.
    Available(bool),
}

/// Menu labels, translated on the main thread (gettext isn't used off it).
#[derive(Clone)]
struct Labels {
    show: String,
    play: String,
    pause: String,
    next: String,
    previous: String,
    quit: String,
    not_playing: String,
}

struct LioTray {
    tx: async_channel::Sender<Command>,
    labels: Labels,
    icon: Vec<ksni::Icon>,
    /// "Title — Artist", or empty when nothing is loaded.
    now_playing: String,
    playing: bool,
}

impl LioTray {
    fn send(&self, command: Command) {
        let _ = self.tx.try_send(command);
    }

    fn show(&self) {
        // KDE may deliver the token just after the click event.
        let token = ksni::current_xdg_activation_token().or_else(|| {
            std::thread::sleep(std::time::Duration::from_millis(1));
            ksni::current_xdg_activation_token()
        });
        self.send(Command::Show(token));
    }

    fn item(label: &str, enabled: bool, command: fn() -> Command) -> MenuItem<Self> {
        StandardItem {
            label: label.replace('_', "__"),
            enabled,
            activate: Box::new(move |tray: &mut Self| tray.send(command())),
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for LioTray {
    fn id(&self) -> String {
        config::APP_ID.into()
    }

    fn title(&self) -> String {
        "Lio".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_name(&self) -> String {
        config::APP_ID.into()
    }

    /// Development builds aren't in the icon theme; hosts use this then.
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Lio".into(),
            description: if self.now_playing.is_empty() {
                self.labels.not_playing.clone()
            } else {
                self.now_playing.clone()
            },
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.show();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let loaded = !self.now_playing.is_empty();
        let l = &self.labels;
        let mut items = Vec::new();
        if loaded {
            items.push(
                StandardItem {
                    label: self.now_playing.replace('_', "__"),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            items.push(MenuItem::Separator);
        }
        items.push(
            StandardItem {
                label: l.show.clone(),
                activate: Box::new(|tray: &mut Self| tray.show()),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        let play_pause = if self.playing { &l.pause } else { &l.play };
        items.push(Self::item(play_pause, loaded, || Command::PlayPause));
        items.push(Self::item(&l.previous, loaded, || Command::Previous));
        items.push(Self::item(&l.next, loaded, || Command::Next));
        items.push(MenuItem::Separator);
        items.push(Self::item(&l.quit, true, || Command::Quit));
        items
    }

    fn watcher_online(&self) {
        self.send(Command::Available(true));
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.send(Command::Available(false));
        // Keep the service: the host may come back (e.g. a shell restart).
        true
    }
}

#[derive(Default)]
struct State {
    handle: Option<ksni::Handle<LioTray>>,
    available: bool,
    /// Keeps Lio running while the icon can bring it back.
    hold: Option<gio::ApplicationHoldGuard>,
}

/// Starts the tray icon and keeps it in step with playback.
pub fn setup(app: &LioApplication, controller: &PlaybackController) {
    let (tx, rx) = async_channel::unbounded();
    let tray = LioTray {
        tx,
        labels: Labels {
            show: gettext("Show Lio"),
            play: gettext("Play"),
            pause: gettext("Pause"),
            next: gettext("Next"),
            previous: gettext("Previous"),
            quit: gettext("Quit"),
            not_playing: gettext("Not playing"),
        },
        icon: app_icon(),
        now_playing: now_playing(controller),
        playing: controller.playing(),
    };
    let state = Rc::new(RefCell::new(State::default()));

    glib::spawn_future_local(glib::clone!(
        #[weak]
        app,
        #[weak]
        controller,
        #[strong]
        state,
        async move {
            // Flatpak can't own the item's well-known name; the watcher accepts the
            // unique name instead.
            let sandboxed = ashpd::is_sandboxed();
            match runtime::run(async move { tray.disable_dbus_name(sandboxed).spawn().await }).await
            {
                Ok(handle) => {
                    tracing::debug!("tray icon registered");
                    let mut st = state.borrow_mut();
                    st.handle = Some(handle);
                    st.available = true;
                }
                Err(e) => tracing::info!("no tray icon: {e}"),
            }
            update_hold(&app, &controller, &state);
        }
    ));

    glib::spawn_future_local(glib::clone!(
        #[weak]
        app,
        #[weak]
        controller,
        #[strong]
        state,
        async move {
            while let Ok(command) = rx.recv().await {
                match command {
                    Command::Show(token) => show_window(&app, token.as_deref()),
                    Command::PlayPause => app.activate_action("play-pause", None),
                    Command::Next => app.activate_action("next", None),
                    Command::Previous => app.activate_action("previous", None),
                    Command::Quit => app.activate_action("quit", None),
                    Command::Available(available) => {
                        state.borrow_mut().available = available;
                        update_hold(&app, &controller, &state);
                    }
                }
            }
        }
    ));

    let sync = glib::clone!(
        #[weak]
        app,
        #[strong]
        state,
        move |c: &PlaybackController| {
            let (text, playing) = (now_playing(c), c.playing());
            if let Some(handle) = state.borrow().handle.clone() {
                runtime::spawn(async move {
                    handle
                        .update(move |tray| {
                            tray.now_playing = text;
                            tray.playing = playing;
                        })
                        .await;
                });
            }
            update_hold(&app, c, &state);
        }
    );
    controller.connect_current_track_notify(sync.clone());
    controller.connect_playing_notify(sync);
}

fn now_playing(controller: &PlaybackController) -> String {
    controller
        .current_track()
        .map(|t| format!("{} — {}", t.title(), t.artist()))
        .unwrap_or_default()
}

/// Holds the app while the icon is shown and a track is loaded (see module docs).
fn update_hold(app: &LioApplication, controller: &PlaybackController, state: &RefCell<State>) {
    let mut st = state.borrow_mut();
    let keep = st.available && controller.current_track().is_some();
    if keep && st.hold.is_none() {
        st.hold = Some(app.hold());
    } else if !keep {
        st.hold = None;
    }
}

/// Raises the window, recreating it if it was closed.
fn show_window(app: &LioApplication, token: Option<&str>) {
    if let Some(win) = app.window() {
        if let Some(token) = token {
            win.set_startup_id(token);
        }
        win.present();
    } else {
        app.present_main_window_with_token(token);
    }
}

/// The app icon as ARGB32 pixmaps for hosts that can't find it by name.
fn app_icon() -> Vec<ksni::Icon> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    let theme = gtk::IconTheme::for_display(&display);
    let renderer = gsk::CairoRenderer::new();
    if renderer.realize(None::<&gdk::Surface>).is_err() {
        return Vec::new();
    }
    let icons = [32, 64]
        .into_iter()
        .filter_map(|size| {
            let paintable = theme.lookup_icon(
                config::APP_ID,
                &[],
                size,
                1,
                gtk::TextDirection::None,
                gtk::IconLookupFlags::empty(),
            );
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, f64::from(size), f64::from(size));
            let node = snapshot.to_node()?;
            let texture = renderer.render_texture(
                &node,
                Some(&gtk::graphene::Rect::new(
                    0.0,
                    0.0,
                    size as f32,
                    size as f32,
                )),
            );
            Some(to_argb(&texture))
        })
        .collect();
    renderer.unrealize();
    icons
}

/// Texture → SNI pixmap: ARGB32 in network byte order.
fn to_argb(texture: &gdk::Texture) -> ksni::Icon {
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::A8r8g8b8);
    let (bytes, stride) = downloader.download_bytes();
    let (w, h) = (texture.width() as usize, texture.height() as usize);
    let mut data = Vec::with_capacity(w * h * 4);
    for row in bytes.chunks(stride).take(h) {
        data.extend_from_slice(&row[..w * 4]);
    }
    ksni::Icon {
        width: w as i32,
        height: h as i32,
        data,
    }
}
