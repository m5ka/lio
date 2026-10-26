//! GObject wrapper around [`AlbumRow`].

use std::cell::{Cell, OnceCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use lio_store::AlbumRow;

/// Offline availability of an album's tracks.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "LioOfflineState")]
pub enum OfflineState {
    #[default]
    None,
    Partial,
    Complete,
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Album)]
    pub struct Album {
        pub row: OnceCell<AlbumRow>,
        #[property(get, set)]
        starred: Cell<bool>,
        #[property(get, set, minimum = 0, maximum = 5)]
        rating: Cell<i32>,
        #[property(get, set, builder(OfflineState::None))]
        offline_state: Cell<OfflineState>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Album {
        const NAME: &'static str = "LioAlbum";
        type Type = super::Album;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Album {}
}

glib::wrapper! {
    pub struct Album(ObjectSubclass<imp::Album>);
}

impl Album {
    pub fn new(row: AlbumRow) -> Self {
        let obj: Self = glib::Object::builder()
            .property("starred", row.starred_at.is_some())
            .property("rating", row.rating.unwrap_or(0) as i32)
            .build();
        obj.imp().row.set(row).expect("row set once");
        obj
    }

    pub fn row(&self) -> &AlbumRow {
        self.imp().row.get().expect("constructed with Album::new")
    }

    pub fn rowid(&self) -> i64 {
        self.row().rowid
    }

    pub fn id(&self) -> &str {
        &self.row().id
    }

    pub fn name(&self) -> &str {
        &self.row().name
    }

    pub fn artist(&self) -> String {
        super::display_artist(self.row().artist_display.as_deref())
    }

    pub fn year(&self) -> Option<i64> {
        self.row().year
    }

    pub fn cover_art_id(&self) -> Option<&str> {
        self.row().cover_art_id.as_deref()
    }
}
