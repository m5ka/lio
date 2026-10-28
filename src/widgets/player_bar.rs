//! Persistent player bar: cover, title/artist, transport, seek, volume.

use std::cell::{OnceCell, RefCell};

use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::prelude::*;
use gtk::{CompositeTemplate, gio, glib};

use crate::covers::SIZE_ROW;
use crate::models::format_duration;
use crate::playback_controller::PlaybackController;
use crate::widgets::cover_view;
use crate::widgets::seek_scale::SeekScale;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/player_bar.ui")]
    pub struct LioPlayerBar {
        #[template_child]
        pub cover_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub cover_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub cover_picture: TemplateChild<gtk::Image>,
        #[template_child]
        pub title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub artist_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub play_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub seek_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub seek_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub seek_adjustment: TemplateChild<gtk::Adjustment>,
        #[template_child]
        pub position_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub duration_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub volume_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub volume_adjustment: TemplateChild<gtk::Adjustment>,
        #[template_child]
        pub end_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub repeat_button: TemplateChild<gtk::Button>,
        pub controller: RefCell<Option<PlaybackController>>,
        pub cancellable: RefCell<Option<gio::Cancellable>>,
        pub accent: RefCell<Option<crate::widgets::tint::Tint>>,
        pub seek: OnceCell<SeekScale>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioPlayerBar {
        const NAME: &'static str = "LioPlayerBar";
        type Type = super::LioPlayerBar;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_css_name("playerbar");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioPlayerBar {
        fn constructed(&self) {
            self.parent_constructed();
            self.cover_bin.set_size_request(44, 44);
        }
    }

    impl WidgetImpl for LioPlayerBar {}
    impl BinImpl for LioPlayerBar {}
}

glib::wrapper! {
    pub struct LioPlayerBar(ObjectSubclass<imp::LioPlayerBar>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioPlayerBar {
    pub fn new(controller: &PlaybackController) -> Self {
        let obj: Self = glib::Object::new();
        obj.attach(controller);
        obj
    }

    /// Compact layout for narrow windows: hides seek bar and volume.
    pub fn set_compact(&self, compact: bool) {
        let imp = self.imp();
        imp.seek_box.set_visible(!compact);
        imp.end_box.set_visible(!compact);
    }

    fn attach(&self, controller: &PlaybackController) {
        let imp = self.imp();
        imp.controller.replace(Some(controller.clone()));

        controller.connect_current_track_notify(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |c| bar.update_track(c)
        ));
        controller.connect_playing_notify(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |c| bar.update_playing(c.playing())
        ));
        controller.connect_position_ms_notify(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |c| bar.update_position(c.position_ms(), c.duration_ms())
        ));
        controller.connect_duration_ms_notify(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |c| bar.update_position(c.position_ms(), c.duration_ms())
        ));

        let seek = SeekScale::attach(
            &imp.seek_scale,
            glib::clone!(
                #[weak(rename_to = bar)]
                self,
                move |secs| bar
                    .imp()
                    .position_label
                    .set_label(&format_duration(secs as i64))
            ),
            glib::clone!(
                #[weak(rename_to = bar)]
                self,
                move |secs| {
                    if let Some(c) = bar.imp().controller.borrow().as_ref() {
                        c.seek_ms((secs * 1000.0) as u64);
                    }
                }
            ),
        );
        let _ = imp.seek.set(seek);

        controller
            .bind_property("volume", &imp.volume_adjustment.get(), "value")
            .bidirectional()
            .sync_create()
            .build();
        controller
            .bind_property("volume", &imp.volume_button.get(), "icon-name")
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

        controller
            .bind_property("repeat", &imp.repeat_button.get(), "icon-name")
            .transform_to(|_, repeat: String| {
                Some(if repeat == "one" {
                    "media-playlist-repeat-song-symbolic"
                } else {
                    "media-playlist-repeat-symbolic"
                })
            })
            .sync_create()
            .build();
        let update_repeat = glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |c: &PlaybackController| {
                let button = &bar.imp().repeat_button;
                let (tooltip, on) = match c.repeat().as_str() {
                    "all" => (gettext("Repeat All"), true),
                    "one" => (gettext("Repeat One"), true),
                    _ => (gettext("Repeat Off"), false),
                };
                button.set_tooltip_text(Some(&tooltip));
                if on {
                    button.add_css_class("accent");
                } else {
                    button.remove_css_class("accent");
                }
            }
        );
        update_repeat(controller);
        controller.connect_repeat_notify(update_repeat);

        self.update_track(controller);
        self.update_playing(controller.playing());
    }

    fn update_track(&self, controller: &PlaybackController) {
        let imp = self.imp();
        match controller.current_track() {
            Some(track) => {
                imp.title_label.set_label(track.title());
                imp.artist_label.set_label(&track.artist());
                cover_view::show_cover(
                    &imp.cover_stack,
                    &*imp.cover_picture,
                    track.cover_art_id(),
                    SIZE_ROW,
                    &imp.cancellable,
                );
                self.update_tint(track.id(), track.cover_art_id());
            }
            None => {
                imp.title_label.set_label("");
                imp.artist_label.set_label("");
                cover_view::show_cover(
                    &imp.cover_stack,
                    &*imp.cover_picture,
                    None,
                    SIZE_ROW,
                    &imp.cancellable,
                );
                self.set_tint(None, None);
            }
        }
    }

    /// Once `cover_id` loads: accents the controls with its most vivid colour and washes
    /// the bar in its average colour, the same way as Now Playing.
    fn update_tint(&self, track_id: &str, cover_id: Option<&str>) {
        let Some(id) = cover_id.filter(|id| !id.is_empty()).map(str::to_owned) else {
            self.set_tint(None, None);
            return;
        };
        let track_id = track_id.to_owned();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            async move {
                let accent = crate::widgets::tint::cover_accent(&id).await;
                let average = crate::widgets::tint::cover_average(&id).await;
                // Skip late results for a track that is no longer playing.
                let current = bar
                    .imp()
                    .controller
                    .borrow()
                    .as_ref()
                    .and_then(|c| c.current_track())
                    .is_some_and(|t| t.id() == track_id);
                if current {
                    bar.set_tint(accent, average);
                }
            }
        ));
    }

    /// `None` falls back to the system accent and the plain bar background. The wash's
    /// glow sits behind the cover, at the bar's bottom left.
    fn set_tint(&self, accent: Option<(u8, u8, u8)>, average: Option<(u8, u8, u8)>) {
        let accent = accent
            .map(crate::widgets::tint::accent_declarations)
            .unwrap_or_default();
        let wash = average
            .map(|colour| crate::widgets::tint::wash_declarations(colour, "3% 70%"))
            .unwrap_or_default();
        self.imp()
            .accent
            .borrow_mut()
            .get_or_insert_with(|| crate::widgets::tint::Tint::new(self))
            .set(&format!("{accent} {wash}"));
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
        if playing {
            button.remove_css_class("play-icon");
        } else {
            button.add_css_class("play-icon");
        }
        button.set_tooltip_text(Some(&label));
        button.update_property(&[gtk::accessible::Property::Label(&label)]);
    }

    fn update_position(&self, position_ms: u64, duration_ms: u64) {
        let imp = self.imp();
        let dur = format_duration((duration_ms / 1000) as i64);
        imp.duration_label.set_label(&dur);
        imp.seek_adjustment
            .set_upper((duration_ms as f64 / 1000.0).max(1.0));
        // Leave knob and label where the user is dragging them.
        if imp.seek.get().is_some_and(SeekScale::is_scrubbing) {
            return;
        }
        imp.seek_adjustment.set_value(position_ms as f64 / 1000.0);
        let pos = format_duration((position_ms / 1000) as i64);
        imp.position_label.set_label(&pos);
        imp.seek_scale
            .update_property(&[gtk::accessible::Property::ValueText(
                &gettext("{} of {}")
                    .replacen("{}", &pos, 1)
                    .replacen("{}", &dur, 1),
            )]);
    }
}
