//! GObject wrapper around [`ArtistRow`].

use std::cell::{Cell, OnceCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use lio_store::ArtistRow;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Artist)]
    pub struct Artist {
        pub row: OnceCell<ArtistRow>,
        #[property(get, set)]
        starred: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Artist {
        const NAME: &'static str = "LioArtist";
        type Type = super::Artist;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Artist {}
}

glib::wrapper! {
    pub struct Artist(ObjectSubclass<imp::Artist>);
}

impl Artist {
    pub fn new(row: ArtistRow) -> Self {
        let obj: Self = glib::Object::builder()
            .property("starred", row.starred_at.is_some())
            .build();
        obj.imp().row.set(row).expect("row set once");
        obj
    }

    pub fn row(&self) -> &ArtistRow {
        self.imp().row.get().expect("constructed with Artist::new")
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

    pub fn cover_art_id(&self) -> Option<&str> {
        self.row().cover_art_id.as_deref()
    }
}
