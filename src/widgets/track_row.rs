//! Track row for album and playlist pages.

use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib};

use crate::models::{Track, format_duration};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/track_row.ui")]
    pub struct LioTrackRow {
        #[template_child]
        pub number_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub number_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub duration_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub star_button: TemplateChild<gtk::Button>,
        pub track: RefCell<Option<Track>>,
        pub bindings: RefCell<Vec<glib::Binding>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioTrackRow {
        const NAME: &'static str = "LioTrackRow";
        type Type = super::LioTrackRow;
        type ParentType = adw::ActionRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioTrackRow {}
    impl WidgetImpl for LioTrackRow {}
    impl ListBoxRowImpl for LioTrackRow {}
    impl PreferencesRowImpl for LioTrackRow {}
    impl ActionRowImpl for LioTrackRow {}
}

glib::wrapper! {
    pub struct LioTrackRow(ObjectSubclass<imp::LioTrackRow>)
        @extends adw::ActionRow, adw::PreferencesRow, gtk::ListBoxRow, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioTrackRow {
    /// A row for `track`. `album_artist` hides the subtitle when it matches the track artist.
    pub fn new(track: &Track, album_artist: Option<&str>) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        let row = track.row();
        obj.set_title(track.title());
        let artist = track.artist();
        if album_artist.is_none_or(|a| a != artist) {
            obj.set_subtitle(&artist);
        }
        imp.number_label
            .set_label(&row.track_no.map(|n| n.to_string()).unwrap_or_default());
        imp.duration_label
            .set_label(&format_duration(row.duration_s));
        obj.update_property(&[gtk::accessible::Property::Label(&format!(
            "{}, {artist}, {}",
            track.title(),
            format_duration(row.duration_s)
        ))]);
        let binding = track
            .bind_property("playing", &imp.number_stack.get(), "visible-child-name")
            .transform_to(|_, playing: bool| Some(if playing { "playing" } else { "number" }))
            .sync_create()
            .build();
        imp.bindings.borrow_mut().push(binding);
        imp.track.replace(Some(track.clone()));
        obj.set_starred(track.starred());
        imp.star_button.set_action_name(Some("item.star"));
        imp.star_button
            .set_action_target_value(Some(&("track", track.id()).to_variant()));
        obj
    }

    pub fn set_starred(&self, starred: bool) {
        crate::widgets::pages::album_page::set_star_icon(&self.imp().star_button, starred);
    }

    pub fn track(&self) -> Option<Track> {
        self.imp().track.borrow().clone()
    }

    pub fn set_playing(&self, playing: bool) {
        if let Some(track) = self.track() {
            track.set_playing(playing);
        }
    }
}
