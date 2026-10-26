//! GObject wrapper around [`PlaylistRow`].

use std::cell::{Cell, OnceCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use lio_store::PlaylistRow;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Playlist)]
    pub struct Playlist {
        pub row: OnceCell<PlaylistRow>,
        #[property(get, set)]
        offline_available: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Playlist {
        const NAME: &'static str = "LioPlaylist";
        type Type = super::Playlist;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Playlist {}
}

glib::wrapper! {
    pub struct Playlist(ObjectSubclass<imp::Playlist>);
}

impl Playlist {
    pub fn new(row: PlaylistRow) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().row.set(row).expect("row set once");
        obj
    }

    pub fn row(&self) -> &PlaylistRow {
        self.imp()
            .row
            .get()
            .expect("constructed with Playlist::new")
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

    pub fn readonly(&self) -> bool {
        self.row().readonly
    }
}
