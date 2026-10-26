//! Track row for virtualised `GtkListView`s (playlists, favourites).

use std::cell::RefCell;

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{CompositeTemplate, gio, glib};

use crate::application::LioApplication;
use crate::covers::SIZE_ROW;
use crate::models::{Track, format_duration};
use crate::widgets::{cover_view, item_menu};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/track_list_row.ui")]
    pub struct LioTrackListRow {
        #[template_child]
        pub cover_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub cover_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub cover_image: TemplateChild<gtk::Image>,
        #[template_child]
        pub playing_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub subtitle_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub duration_label: TemplateChild<gtk::Label>,
        pub track: RefCell<Option<Track>>,
        pub cancellable: RefCell<Option<gio::Cancellable>>,
        pub playing_handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioTrackListRow {
        const NAME: &'static str = "LioTrackListRow";
        type Type = super::LioTrackListRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioTrackListRow {
        fn constructed(&self) {
            self.parent_constructed();
            self.cover_bin.set_size_request(40, 40);
            item_menu::attach(&*self.obj(), |row: &super::LioTrackListRow| {
                row.track().map(|t| item_menu::Target::Track {
                    id: t.id().to_owned(),
                    album_id: t.row().album_id.clone(),
                })
            });
        }

        fn dispose(&self) {
            self.obj().unbind();
        }
    }

    impl WidgetImpl for LioTrackListRow {}
    impl BoxImpl for LioTrackListRow {}
}

glib::wrapper! {
    pub struct LioTrackListRow(ObjectSubclass<imp::LioTrackListRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl LioTrackListRow {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn track(&self) -> Option<Track> {
        self.imp().track.borrow().clone()
    }

    pub fn bind(&self, track: &Track) {
        let imp = self.imp();
        imp.title_label.set_label(track.title());
        let artist = track.artist();
        let subtitle = match track.album() {
            Some(album) if !album.is_empty() => format!("{artist} — {album}"),
            _ => artist.clone(),
        };
        imp.subtitle_label.set_label(&subtitle);
        imp.duration_label
            .set_label(&format_duration(track.duration_s()));
        self.set_opacity(
            if LioApplication::default().session().is_online() || track.offline_available() {
                1.0
            } else {
                0.5
            },
        );
        self.update_property(&[gtk::accessible::Property::Label(&format!(
            "{}, {artist}, {}",
            track.title(),
            format_duration(track.duration_s())
        ))]);
        cover_view::show_cover(
            &imp.cover_stack,
            &*imp.cover_image,
            track.cover_art_id(),
            SIZE_ROW,
            &imp.cancellable,
        );
        imp.track.replace(Some(track.clone()));

        let playback = LioApplication::default().playback();
        let update = glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |c: &crate::playback_controller::PlaybackController| {
                let current = c.current_track().map(|t| t.rowid());
                let is_current = row.track().is_some_and(|t| Some(t.rowid()) == current);
                row.imp().playing_icon.set_visible(is_current);
            }
        );
        update(&playback);
        imp.playing_handler
            .replace(Some(playback.connect_current_track_notify(update)));
    }

    pub fn unbind(&self) {
        let imp = self.imp();
        if let Some(id) = imp.playing_handler.take() {
            LioApplication::default().playback().disconnect(id);
        }
        if let Some(c) = imp.cancellable.take() {
            c.cancel();
        }
        imp.track.replace(None);
    }
}

impl Default for LioTrackListRow {
    fn default() -> Self {
        Self::new()
    }
}

/// A list item factory producing [`LioTrackListRow`]s for [`Track`] models.
pub fn factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        item.set_child(Some(&LioTrackListRow::new()));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let (Some(track), Some(row)) = (
            item.item().and_downcast::<Track>(),
            item.child().and_downcast::<LioTrackListRow>(),
        ) {
            row.bind(&track);
        }
    });
    factory.connect_unbind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let Some(row) = item.child().and_downcast::<LioTrackListRow>() {
            row.unbind();
        }
    });
    factory
}
