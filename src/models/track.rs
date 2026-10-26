//! GObject wrapper around [`TrackRow`] for list models.

use std::cell::{Cell, OnceCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use lio_store::TrackRow;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Track)]
    pub struct Track {
        pub row: OnceCell<TrackRow>,
        #[property(get, set)]
        starred: Cell<bool>,
        #[property(get, set, minimum = 0, maximum = 5)]
        rating: Cell<i32>,
        #[property(get, set)]
        offline_available: Cell<bool>,
        /// The track is the current queue item.
        #[property(get, set)]
        playing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Track {
        const NAME: &'static str = "LioTrack";
        type Type = super::Track;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Track {}
}

glib::wrapper! {
    pub struct Track(ObjectSubclass<imp::Track>);
}

impl Track {
    pub fn new(row: TrackRow) -> Self {
        let obj: Self = glib::Object::builder()
            .property("starred", row.starred_at.is_some())
            .property("rating", row.rating.unwrap_or(0) as i32)
            .property("offline-available", row.offline_available)
            .build();
        obj.imp().row.set(row).expect("row set once");
        obj
    }

    pub fn row(&self) -> &TrackRow {
        self.imp().row.get().expect("constructed with Track::new")
    }

    pub fn rowid(&self) -> i64 {
        self.row().rowid
    }

    pub fn id(&self) -> &str {
        &self.row().id
    }

    pub fn title(&self) -> &str {
        &self.row().title
    }

    /// Artist, or a translated "Unknown Artist" placeholder.
    pub fn artist(&self) -> String {
        super::display_artist(self.row().artist_display.as_deref())
    }

    pub fn album(&self) -> Option<&str> {
        self.row().album_name.as_deref()
    }

    pub fn duration_s(&self) -> i64 {
        self.row().duration_s
    }

    pub fn cover_art_id(&self) -> Option<&str> {
        self.row().cover_art_id.as_deref()
    }
}
