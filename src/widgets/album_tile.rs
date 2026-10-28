//! Album grid tile: cover, title, artist.

use std::cell::RefCell;

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{CompositeTemplate, gio, glib};

use crate::covers::SIZE_GRID;
use crate::models::Album;
use crate::widgets::cover_view;

/// Displayed cover edge in pixels.
pub const TILE_COVER: i32 = 160;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/album_tile.ui")]
    pub struct LioAlbumTile {
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
        pub offline_badge: TemplateChild<gtk::Image>,
        pub album: RefCell<Option<Album>>,
        pub cancellable: RefCell<Option<gio::Cancellable>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioAlbumTile {
        const NAME: &'static str = "LioAlbumTile";
        type Type = super::LioAlbumTile;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioAlbumTile {
        fn constructed(&self) {
            self.parent_constructed();
            self.cover_bin.set_size_request(TILE_COVER, TILE_COVER);
            self.title_label.set_max_width_chars(1);
            self.obj().set_width_request(TILE_COVER);
        }
    }

    impl WidgetImpl for LioAlbumTile {}
    impl BoxImpl for LioAlbumTile {}
}

glib::wrapper! {
    pub struct LioAlbumTile(ObjectSubclass<imp::LioAlbumTile>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl LioAlbumTile {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Shrinks or grows the cover from the default [`TILE_COVER`].
    pub fn set_cover_size(&self, size: i32) {
        let imp = self.imp();
        imp.cover_bin.set_size_request(size, size);
        imp.cover_picture.set_pixel_size(size);
        self.set_width_request(size);
    }

    pub fn album(&self) -> Option<Album> {
        self.imp().album.borrow().clone()
    }

    pub fn bind(&self, album: &Album) {
        let imp = self.imp();
        let artist = album.artist();
        imp.title_label.set_label(album.name());
        imp.title_label.set_tooltip_text(Some(album.name()));
        imp.artist_label.set_label(&artist);
        self.update_property(&[gtk::accessible::Property::Label(&format!(
            "{}, {artist}",
            album.name()
        ))]);
        cover_view::show_cover(
            &imp.cover_stack,
            &*imp.cover_picture,
            album.cover_art_id(),
            SIZE_GRID,
            &imp.cancellable,
        );
        let downloaded = crate::application::LioApplication::default()
            .offline()
            .pin_state(lio_store::downloads::PinKind::Album, album.id())
            .is_some_and(|p| p.available_offline());
        imp.offline_badge.set_visible(downloaded);
        // Offline, albums with nothing playable are dimmed.
        let app = crate::application::LioApplication::default();
        let dim = !app.session().is_online()
            && !downloaded
            && app.session().account().is_some_and(|a| {
                a.db.read(|c| lio_store::downloads::album_offline_state(c, album.rowid()))
                    .is_ok_and(|s| s == lio_store::downloads::OfflineState::None)
            });
        self.set_opacity(if dim { 0.45 } else { 1.0 });
        imp.album.replace(Some(album.clone()));
    }

    pub fn unbind(&self) {
        let imp = self.imp();
        if let Some(c) = imp.cancellable.take() {
            c.cancel();
        }
        imp.album.replace(None);
    }
}

impl Default for LioAlbumTile {
    fn default() -> Self {
        Self::new()
    }
}
