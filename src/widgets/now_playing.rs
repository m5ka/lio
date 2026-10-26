//! Full-window "Now Playing" screen: large cover on the left; title, links,
//! lyrics, seek and transport on the right; the background tinted from the cover. It
//! replaces the window's usual content (sidebars and player bar), see
//! [`LioWindow::show_now_playing`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{CompositeTemplate, gdk, gio, glib};

use crate::application::LioApplication;
use crate::covers::SIZE_FULL;
use crate::lyrics::Lyrics;
use crate::models::format_duration;
use crate::playback_controller::PlaybackController;
use crate::widgets::{LioWindow, cover_view, navigate, pages::album_page::set_star_icon};

/// Tallest the lyrics panel gets beside the cover (the cover is at most 640 px).
const LYRICS_MAX_HEIGHT: i32 = 320;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/now_playing.ui")]
    pub struct LioNowPlaying {
        #[template_child]
        pub narrow_breakpoint: TemplateChild<adw::Breakpoint>,
        #[template_child]
        pub cover_clamp: TemplateChild<adw::Clamp>,
        #[template_child]
        pub cover_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub cover_picture: TemplateChild<gtk::Picture>,
        #[template_child]
        pub info_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub artist_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub album_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub format_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub star_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub rating_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub volume_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub fullscreen_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub volume_adjustment: TemplateChild<gtk::Adjustment>,
        #[template_child]
        pub lyrics_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub lyrics_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub lyrics_scroller: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub lyrics_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub seek_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub seek_adjustment: TemplateChild<gtk::Adjustment>,
        #[template_child]
        pub position_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub remaining_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub play_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub repeat_button: TemplateChild<gtk::Button>,
        pub controller: RefCell<Option<PlaybackController>>,
        pub handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
        pub cancellable: RefCell<Option<gio::Cancellable>>,
        pub tint: RefCell<Option<gtk::CssProvider>>,
        pub updating: Cell<bool>,
        pub narrow: Cell<bool>,
        /// Track the lyrics belong to; guards against late results for a skipped track.
        pub lyrics_track: RefCell<Option<String>>,
        pub lyrics: RefCell<Option<Rc<Lyrics>>>,
        /// A lookup for `lyrics_track` is running.
        pub lyrics_loading: Cell<bool>,
        /// The last lookup for `lyrics_track` reached every source.
        pub lyrics_complete: Cell<bool>,
        pub lyrics_attempts: Cell<usize>,
        pub lyrics_retry: RefCell<Option<glib::SourceId>>,
        pub lyrics_labels: RefCell<Vec<gtk::Label>>,
        pub current_line: Cell<Option<usize>>,
        pub scroll_animation: RefCell<Option<adw::TimedAnimation>>,
        /// `notify::fullscreened` on the window this screen is shown in.
        pub fullscreen_handler: RefCell<Option<(gtk::Window, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioNowPlaying {
        const NAME: &'static str = "LioNowPlaying";
        type Type = super::LioNowPlaying;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.install_action("now-playing.close", None, |screen, _, _| {
                if let Some(win) = screen.window() {
                    win.hide_now_playing();
                }
            });
            klass.install_action_async("now-playing.star", None, |screen, _, _| async move {
                let id = screen.current_id();
                if let (Some(id), Some(win)) = (id, screen.window()) {
                    crate::actions::annotation::toggle_star(&win, "track", &id).await;
                    screen.update_star();
                }
            });
            klass.install_action_async("now-playing.go-album", None, |screen, _, _| async move {
                let album = screen
                    .controller()
                    .and_then(|c| c.current_track())
                    .and_then(|t| t.row().album_id.clone());
                if let (Some(id), Some(win)) = (album, screen.window()) {
                    win.hide_now_playing();
                    navigate::album(&win, &id).await;
                }
            });
            klass.install_action("now-playing.go-artist", None, |screen, _, _| {
                let (Some(track_id), Some(win)) = (screen.current_id(), screen.window()) else {
                    return;
                };
                win.hide_now_playing();
                WidgetExt::activate_action(
                    &win,
                    "item.go-artist",
                    Some(&("track", track_id.as_str()).to_variant()),
                )
                .ok();
            });
            klass.install_action("now-playing.fullscreen", None, |screen, _, _| {
                if let Some(win) = screen.window() {
                    win.set_fullscreened(!win.is_fullscreen());
                }
            });
            // Escape leaves fullscreen first, then closes the screen.
            klass.install_action("now-playing.escape", None, |screen, _, _| {
                if let Some(win) = screen.window() {
                    if win.is_fullscreen() {
                        win.unfullscreen();
                    } else {
                        win.hide_now_playing();
                    }
                }
            });
            klass.add_binding_action(
                gdk::Key::Escape,
                gdk::ModifierType::empty(),
                "now-playing.escape",
            );
            klass.add_binding_action(
                gdk::Key::F11,
                gdk::ModifierType::empty(),
                "now-playing.fullscreen",
            );
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioNowPlaying {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            for (obj, id) in self.handlers.take() {
                obj.disconnect(id);
            }
            if let Some(source) = self.lyrics_retry.take() {
                source.remove();
            }
            if let Some(provider) = self.tint.take() {
                gtk::style_context_remove_provider_for_display(&self.obj().display(), &provider);
            }
        }
    }

    impl WidgetImpl for LioNowPlaying {
        fn root(&self) {
            self.parent_root();
            let obj = self.obj();
            if let Some(win) = obj.root().and_downcast::<gtk::Window>() {
                let id = win.connect_fullscreened_notify(glib::clone!(
                    #[weak]
                    obj,
                    move |win| obj.update_fullscreen(win.is_fullscreen())
                ));
                obj.update_fullscreen(win.is_fullscreen());
                self.fullscreen_handler.replace(Some((win, id)));
            }
        }

        fn unroot(&self) {
            if let Some((win, id)) = self.fullscreen_handler.take() {
                win.disconnect(id);
            }
            self.parent_unroot();
        }
    }

    impl BinImpl for LioNowPlaying {}
}

glib::wrapper! {
    pub struct LioNowPlaying(ObjectSubclass<imp::LioNowPlaying>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioNowPlaying {
    pub fn new(controller: &PlaybackController) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().controller.replace(Some(controller.clone()));
        obj.attach(controller);
        obj
    }

    fn window(&self) -> Option<LioWindow> {
        self.root().and_downcast()
    }

    fn controller(&self) -> Option<PlaybackController> {
        self.imp().controller.borrow().clone()
    }

    fn current_id(&self) -> Option<String> {
        self.controller()
            .and_then(|c| c.current_track())
            .map(|t| t.id().to_owned())
    }

    fn setup(&self) {
        let imp = self.imp();
        imp.narrow_breakpoint.connect_apply(glib::clone!(
            #[weak(rename_to = s)]
            self,
            move |_| {
                s.imp().narrow.set(true);
                s.update_layout();
            }
        ));
        imp.narrow_breakpoint.connect_unapply(glib::clone!(
            #[weak(rename_to = s)]
            self,
            move |_| {
                s.imp().narrow.set(false);
                s.update_layout();
            }
        ));
        LioApplication::default()
            .settings()
            .bind("now-playing-lyrics", &imp.lyrics_button.get(), "active")
            .build();
        for prop in ["active", "visible"] {
            imp.lyrics_button.connect_notify_local(
                Some(prop),
                glib::clone!(
                    #[weak(rename_to = s)]
                    self,
                    move |_, _| s.update_layout()
                ),
            );
        }
        // Scroll once the panel has its final size.
        imp.lyrics_revealer
            .connect_child_revealed_notify(glib::clone!(
                #[weak(rename_to = s)]
                self,
                move |r| {
                    if r.is_child_revealed() {
                        s.scroll_to_current(false);
                    } else if !r.reveals_child() {
                        // Fully closed: drop out of the box so it adds no spacing.
                        r.set_visible(false);
                    }
                }
            ));
        self.update_layout();
    }

    fn lyrics_open(&self) -> bool {
        let button = &self.imp().lyrics_button;
        button.is_visible() && button.is_active()
    }

    /// Side by side, the lyrics panel is capped so the details column stays about as
    /// tall as the cover. On narrow windows open lyrics replace the cover, which would
    /// otherwise squeeze them, and take the free height instead.
    fn update_layout(&self) {
        let imp = self.imp();
        let open = self.lyrics_open();
        let narrow = imp.narrow.get();
        if open {
            imp.lyrics_revealer.set_visible(true);
        }
        imp.lyrics_revealer.set_reveal_child(open);
        imp.lyrics_revealer.set_vexpand(open && narrow);
        imp.lyrics_scroller.set_vexpand(open && narrow);
        imp.lyrics_scroller
            .set_max_content_height(if narrow { -1 } else { LYRICS_MAX_HEIGHT });
        imp.info_box.set_vexpand(open && narrow);
        imp.info_box.set_valign(if open && narrow {
            gtk::Align::Fill
        } else {
            gtk::Align::Center
        });
        imp.cover_clamp.set_visible(!(open && narrow));
    }

    fn attach(&self, c: &PlaybackController) {
        let imp = self.imp();
        let mut handlers = Vec::new();
        let obj: glib::Object = c.clone().upcast();
        handlers.push((
            obj.clone(),
            c.connect_current_track_notify(glib::clone!(
                #[weak(rename_to = s)]
                self,
                move |c| s.update_track(c)
            )),
        ));
        handlers.push((
            obj.clone(),
            c.connect_playing_notify(glib::clone!(
                #[weak(rename_to = s)]
                self,
                move |c| s.update_playing(c.playing())
            )),
        ));
        handlers.push((
            obj.clone(),
            c.connect_position_ms_notify(glib::clone!(
                #[weak(rename_to = s)]
                self,
                move |c| s.update_position(c.position_ms(), c.duration_ms())
            )),
        ));
        handlers.push((
            obj.clone(),
            c.connect_repeat_notify(glib::clone!(
                #[weak(rename_to = s)]
                self,
                move |c| s.update_repeat(&c.repeat())
            )),
        ));
        let session = LioApplication::default().session();
        handlers.push((
            session.clone().upcast(),
            session.connect_state_notify(glib::clone!(
                #[weak(rename_to = s)]
                self,
                move |session| {
                    if session.is_online() {
                        s.retry_lyrics();
                    }
                }
            )),
        ));
        imp.handlers.replace(handlers);
        // Opening the screen is another chance for a lookup that failed.
        self.connect_map(|s| s.retry_lyrics());
        imp.seek_adjustment.connect_value_changed(glib::clone!(
            #[weak(rename_to = s)]
            self,
            move |adj| {
                if !s.imp().updating.get()
                    && let Some(c) = s.controller()
                {
                    c.seek_ms((adj.value() * 1000.0) as u64);
                }
            }
        ));
        c.bind_property("volume", &imp.volume_adjustment.get(), "value")
            .bidirectional()
            .sync_create()
            .build();
        c.bind_property("volume", &imp.volume_button.get(), "icon-name")
            .transform_to(|_, v: f64| {
                Some(match v {
                    v if v <= 0.0 => "audio-volume-muted-symbolic",
                    v if v < 0.34 => "audio-volume-low-symbolic",
                    v if v < 0.67 => "audio-volume-medium-symbolic",
                    _ => "audio-volume-high-symbolic",
                })
            })
            .sync_create()
            .build();
        self.update_track(c);
        self.update_playing(c.playing());
        self.update_position(c.position_ms(), c.duration_ms());
        self.update_repeat(&c.repeat());
    }

    fn update_track(&self, c: &PlaybackController) {
        let imp = self.imp();
        let Some(track) = c.current_track() else {
            if let Some(win) = self.window() {
                win.hide_now_playing();
            }
            return;
        };
        let row = track.row();
        imp.title_label.set_label(track.title());
        imp.artist_label.set_label(&track.artist());
        imp.album_label
            .set_label(row.album_name.as_deref().unwrap_or_default());
        let badge = format_badge(row, c.stream_quality());
        imp.format_label.set_visible(!badge.is_empty());
        imp.format_label.set_label(&badge);
        cover_view::show_cover(
            &imp.cover_stack,
            &*imp.cover_picture,
            track.cover_art_id(),
            SIZE_FULL,
            &imp.cancellable,
        );
        self.update_star();
        self.update_rating_menu(track.id());
        if let (Some(id), Some(covers)) = (
            track.cover_art_id().map(str::to_owned),
            LioApplication::default().covers(),
        ) {
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = s)]
                self,
                async move {
                    if let Some(texture) = covers.load(&id, SIZE_FULL, None).await {
                        s.apply_tint(&texture);
                    }
                }
            ));
        }
        self.load_lyrics(&track);
    }

    /// Looks up lyrics for `track`. For the track already shown this only retries an
    /// incomplete lookup (see [`crate::lyrics::Lookup::complete`]).
    fn load_lyrics(&self, track: &crate::models::Track) {
        let imp = self.imp();
        let id = track.id().to_owned();
        if imp.lyrics_track.borrow().as_deref() == Some(id.as_str()) {
            if imp.lyrics_complete.get() || imp.lyrics_loading.get() {
                return;
            }
        } else {
            imp.lyrics_track.replace(Some(id.clone()));
            imp.lyrics_attempts.set(0);
            imp.lyrics_complete.set(false);
            self.show_lyrics(None);
        }
        if let Some(source) = imp.lyrics_retry.take() {
            source.remove();
        }
        imp.lyrics_loading.set(true);
        imp.lyrics_attempts.set(imp.lyrics_attempts.get() + 1);
        let track = track.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = s)]
            self,
            async move {
                let lookup = crate::lyrics::load(&track).await;
                let imp = s.imp();
                if imp.lyrics_track.borrow().as_deref() != Some(id.as_str()) {
                    return;
                }
                imp.lyrics_loading.set(false);
                imp.lyrics_complete.set(lookup.complete);
                // A retry that finds nothing keeps what an earlier attempt found.
                if lookup.lyrics.is_some() || imp.lyrics.borrow().is_none() {
                    s.show_lyrics(lookup.lyrics);
                }
                if !lookup.complete {
                    s.schedule_lyrics_retry();
                }
            }
        ));
    }

    /// Retries an incomplete lookup after 5 s, 15 s, 45 s and 2 min.
    fn schedule_lyrics_retry(&self) {
        const DELAYS: [u64; 4] = [5, 15, 45, 120];
        let imp = self.imp();
        let Some(delay) = DELAYS.get(imp.lyrics_attempts.get() - 1) else {
            return;
        };
        let source = glib::timeout_add_seconds_local_once(
            *delay as u32,
            glib::clone!(
                #[weak(rename_to = s)]
                self,
                move || {
                    s.imp().lyrics_retry.take();
                    s.retry_lyrics();
                }
            ),
        );
        imp.lyrics_retry.replace(Some(source));
    }

    /// Retries the current track's lookup if the last one was incomplete.
    fn retry_lyrics(&self) {
        if let Some(track) = self.controller().and_then(|c| c.current_track()) {
            self.load_lyrics(&track);
        }
    }

    fn show_lyrics(&self, lyrics: Option<Rc<Lyrics>>) {
        let imp = self.imp();
        while let Some(child) = imp.lyrics_box.first_child() {
            imp.lyrics_box.remove(&child);
        }
        imp.lyrics_labels.borrow_mut().clear();
        imp.current_line.set(None);
        imp.lyrics_button.set_visible(lyrics.is_some());
        let Some(lyrics) = lyrics else {
            imp.lyrics.replace(None);
            return;
        };
        let mut labels = Vec::with_capacity(lyrics.lines.len());
        for line in &lyrics.lines {
            let text = if line.text.is_empty() && lyrics.synced {
                "♪"
            } else {
                line.text.as_str()
            };
            let label = gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(!lyrics.synced)
                .css_classes([if lyrics.synced {
                    "lyrics-line"
                } else {
                    "lyrics-plain"
                }])
                .build();
            if let Some(start) = line.start_ms {
                // Clicking a synced line jumps there.
                label.set_cursor_from_name(Some("pointer"));
                let click = gtk::GestureClick::new();
                click.connect_released(glib::clone!(
                    #[weak(rename_to = s)]
                    self,
                    move |_, _, _, _| {
                        if let Some(c) = s.controller() {
                            c.seek_ms(start);
                        }
                    }
                ));
                label.add_controller(click);
            }
            imp.lyrics_box.append(&label);
            labels.push(label);
        }
        if lyrics.source == crate::lyrics::Source::Lrclib {
            let credit = gtk::Label::builder()
                .label(gettext("Lyrics from LRCLIB"))
                .xalign(0.0)
                .margin_top(12)
                .css_classes(["caption", "dim-label"])
                .build();
            imp.lyrics_box.append(&credit);
        }
        imp.lyrics_labels.replace(labels);
        imp.lyrics.replace(Some(lyrics));
        imp.lyrics_scroller.vadjustment().set_value(0.0);
        if let Some(c) = self.controller() {
            self.update_current_line(c.position_ms());
        }
    }

    /// Highlights the line being sung and keeps it in view.
    fn update_current_line(&self, position_ms: u64) {
        let imp = self.imp();
        let current = imp
            .lyrics
            .borrow()
            .as_ref()
            .and_then(|l| l.current_line(position_ms));
        let previous = imp.current_line.replace(current);
        if previous == current {
            return;
        }
        let labels = imp.lyrics_labels.borrow();
        if let Some(label) = previous.and_then(|i| labels.get(i)) {
            label.remove_css_class("current");
        }
        if let Some(label) = current.and_then(|i| labels.get(i)) {
            label.add_css_class("current");
        }
        drop(labels);
        self.scroll_to_current(true);
    }

    fn scroll_to_current(&self, animate: bool) {
        let imp = self.imp();
        if !self.lyrics_open() {
            return;
        }
        let Some(label) = imp
            .current_line
            .get()
            .and_then(|i| imp.lyrics_labels.borrow().get(i).cloned())
        else {
            return;
        };
        let Some(point) = label.compute_point(&*imp.lyrics_box, &gtk::graphene::Point::zero())
        else {
            return;
        };
        let adj = imp.lyrics_scroller.vadjustment();
        // Keep the current line about a third of the way down.
        let target = (f64::from(point.y()) - adj.page_size() / 3.0).clamp(
            adj.lower(),
            (adj.upper() - adj.page_size()).max(adj.lower()),
        );
        if let Some(previous) = imp.scroll_animation.take() {
            previous.skip();
        }
        if !animate || !adw::is_animations_enabled(self) {
            adj.set_value(target);
            return;
        }
        let animation = adw::TimedAnimation::new(
            &*imp.lyrics_scroller,
            adj.value(),
            target,
            400,
            adw::PropertyAnimationTarget::new(&adj, "value"),
        );
        animation.set_easing(adw::Easing::EaseOutCubic);
        animation.play();
        imp.scroll_animation.replace(Some(animation));
    }

    fn update_fullscreen(&self, fullscreen: bool) {
        let imp = self.imp();
        // No window controls on a fullscreen window.
        imp.header_bar.set_show_start_title_buttons(!fullscreen);
        imp.header_bar.set_show_end_title_buttons(!fullscreen);
        let button = &imp.fullscreen_button;
        let label = if fullscreen {
            gettext("Leave Fullscreen")
        } else {
            gettext("Fullscreen")
        };
        button.set_icon_name(if fullscreen {
            "view-restore-symbolic"
        } else {
            "view-fullscreen-symbolic"
        });
        button.set_tooltip_text(Some(&label));
        button.update_property(&[gtk::accessible::Property::Label(&label)]);
    }

    fn update_star(&self) {
        let starred = self
            .current_id()
            .is_some_and(|id| crate::actions::annotation::is_starred("track", &id));
        set_star_icon(&self.imp().star_button, starred);
    }

    fn update_rating_menu(&self, track_id: &str) {
        let current = crate::actions::annotation::rating(track_id).unwrap_or(0);
        let menu = gio::Menu::new();
        let section = gio::Menu::new();
        for n in 1..=5i32 {
            let label = format!("{}{}", "★".repeat(n as usize), "☆".repeat(5 - n as usize));
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(Some("item.rate"), Some(&(track_id, n).to_variant()));
            section.append_item(&item);
        }
        if current > 0 {
            let clear = gio::MenuItem::new(Some(&gettext("_Clear Rating")), None);
            clear.set_action_and_target_value(
                Some("item.rate"),
                Some(&(track_id, 0i32).to_variant()),
            );
            section.append_item(&clear);
        }
        menu.append_section(Some(&gettext("Rating")), &section);
        let add = gio::MenuItem::new(Some(&gettext("Add to _Playlist…")), None);
        add.set_action_and_target_value(
            Some("item.add-to-playlist"),
            Some(&("track", track_id).to_variant()),
        );
        menu.append_item(&add);
        let imp = self.imp();
        imp.rating_button.set_menu_model(Some(&menu));
        imp.rating_button.set_tooltip_text(Some(&if current > 0 {
            gettext("Rated {} of 5").replace("{}", &current.to_string())
        } else {
            gettext("More")
        }));
    }

    fn update_playing(&self, playing: bool) {
        let button = &self.imp().play_button;
        let label = if playing {
            gettext("Pause")
        } else {
            gettext("Play")
        };
        button.set_icon_name(if playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        button.set_tooltip_text(Some(&label));
        button.update_property(&[gtk::accessible::Property::Label(&label)]);
    }

    fn update_repeat(&self, repeat: &str) {
        let button = &self.imp().repeat_button;
        button.set_icon_name(if repeat == "one" {
            "media-playlist-repeat-song-symbolic"
        } else {
            "media-playlist-repeat-symbolic"
        });
        if repeat == "off" {
            button.remove_css_class("accent");
        } else {
            button.add_css_class("accent");
        }
    }

    fn update_position(&self, position_ms: u64, duration_ms: u64) {
        let imp = self.imp();
        imp.updating.set(true);
        imp.seek_adjustment
            .set_upper((duration_ms as f64 / 1000.0).max(1.0));
        imp.seek_adjustment.set_value(position_ms as f64 / 1000.0);
        imp.updating.set(false);
        let pos = format_duration((position_ms / 1000) as i64);
        let remaining = format_duration((duration_ms.saturating_sub(position_ms) / 1000) as i64);
        imp.position_label.set_label(&pos);
        imp.remaining_label.set_label(&format!("−{remaining}"));
        imp.seek_scale
            .update_property(&[gtk::accessible::Property::ValueText(
                &gettext("{} of {}").replacen("{}", &pos, 1).replacen(
                    "{}",
                    &format_duration((duration_ms / 1000) as i64),
                    1,
                ),
            )]);
        self.update_current_line(position_ms);
    }

    /// Fills the screen with a translucent-looking wash of the cover's colour: the
    /// colour mixed into the window background, plus a glow behind the cover.
    fn apply_tint(&self, texture: &gdk::Texture) {
        let Some((r, g, b)) = crate::widgets::tint::average_colour(texture) else {
            return;
        };
        let css = format!(
            ".now-playing {{
               background-color: color-mix(in srgb, rgb({r},{g},{b}) 30%, var(--window-bg-color));
               background-image:
                 radial-gradient(ellipse at 22% 45%, rgba({r},{g},{b},0.5), transparent 60%),
                 linear-gradient(to bottom, rgba({r},{g},{b},0.18), transparent 70%);
             }}"
        );
        let imp = self.imp();
        let provider = imp
            .tint
            .borrow_mut()
            .get_or_insert_with(|| {
                let p = gtk::CssProvider::new();
                gtk::style_context_add_provider_for_display(
                    &self.display(),
                    &p,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
                p
            })
            .clone();
        provider.load_from_string(&css);
    }
}

/// "FLAC · 16 bit · 44.1 kHz", or the transcoding quality while streaming transcoded.
fn format_badge(row: &lio_store::TrackRow, quality: lio_store::Quality) -> String {
    if quality.is_transcoded() && !row.offline_available {
        let (format, rate) = quality.stream_params();
        return format!(
            "{} · {} kbps",
            format.to_uppercase(),
            rate.unwrap_or_default()
        );
    }
    let mut parts = Vec::new();
    if let Some(suffix) = &row.suffix {
        parts.push(suffix.to_uppercase());
    }
    if let Some(bits) = row.bit_depth.filter(|b| *b > 0) {
        parts.push(format!("{bits} bit"));
    }
    if let Some(rate) = row.sample_rate.filter(|r| *r > 0) {
        parts.push(format!("{:.1} kHz", rate as f64 / 1000.0).replace(".0 kHz", " kHz"));
    } else if let Some(kbps) = row.bitrate_kbps.filter(|b| *b > 0) {
        parts.push(format!("{kbps} kbps"));
    }
    parts.join(" · ")
}
