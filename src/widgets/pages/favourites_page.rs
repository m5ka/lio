//! Favourites: starred albums, artists and songs.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib};
use lio_store::queries::{self, AlbumFilter, AlbumSort};

use crate::application::LioApplication;
use crate::models::{Album, Artist, IdListModel, ItemKind};
use crate::playback_controller::QueueOrigin;
use crate::widgets::{
    LioAlbumPage, LioAlbumTile, LioArtistPage, LioListRow, item_menu, track_list_row,
};

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/favourites_page.ui")]
    pub struct LioFavouritesPage {
        #[template_child]
        pub albums_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub albums_grid: TemplateChild<gtk::GridView>,
        #[template_child]
        pub artists_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub artists_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub tracks_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub tracks_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub switcher_bar: TemplateChild<adw::ViewSwitcherBar>,
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        pub models: RefCell<Option<(IdListModel, IdListModel, IdListModel)>>,
        pub handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioFavouritesPage {
        const NAME: &'static str = "LioFavouritesPage";
        type Type = super::LioFavouritesPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioFavouritesPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            if let Some(id) = self.handler.take() {
                LioApplication::default().library().disconnect(id);
            }
        }
    }

    impl WidgetImpl for LioFavouritesPage {}
    impl NavigationPageImpl for LioFavouritesPage {}
}

glib::wrapper! {
    pub struct LioFavouritesPage(ObjectSubclass<imp::LioFavouritesPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioFavouritesPage {
    pub fn new() -> Self {
        glib::Object::new()
    }

    fn setup(&self) {
        let imp = self.imp();
        let app = LioApplication::default();
        let Some(account) = app.session().account() else {
            return;
        };
        let albums = IdListModel::new(ItemKind::Album, &account.db);
        let artists = IdListModel::new(ItemKind::Artist, &account.db);
        let tracks = IdListModel::new(ItemKind::Track, &account.db);

        let album_factory = gtk::SignalListItemFactory::new();
        album_factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let tile = LioAlbumTile::new();
            item_menu::attach(&tile, |tile: &LioAlbumTile| {
                tile.album()
                    .map(|a| item_menu::Target::Album(a.id().to_owned(), a.row().artist_id.clone()))
            });
            item.set_child(Some(&tile));
        });
        album_factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            if let (Some(album), Some(tile)) = (
                item.item().and_downcast::<Album>(),
                item.child().and_downcast::<LioAlbumTile>(),
            ) {
                tile.bind(&album);
            }
        });
        album_factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            if let Some(tile) = item.child().and_downcast::<LioAlbumTile>() {
                tile.unbind();
            }
        });
        imp.albums_grid.set_factory(Some(&album_factory));
        imp.albums_grid
            .set_model(Some(&gtk::NoSelection::new(Some(albums.clone()))));
        imp.albums_grid.connect_activate(|grid, pos| {
            let album = grid
                .model()
                .and_then(|m| m.item(pos))
                .and_downcast::<Album>();
            if let (Some(album), Some(win)) = (album, LioApplication::default().window()) {
                win.push_page(&LioAlbumPage::new(&album));
            }
        });

        let artist_factory = gtk::SignalListItemFactory::new();
        artist_factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = LioListRow::new();
            item_menu::attach(&row, |row: &LioListRow| {
                row.data()
                    .and_downcast::<Artist>()
                    .map(|a| item_menu::Target::Artist(a.id().to_owned()))
            });
            item.set_child(Some(&row));
        });
        artist_factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            if let (Some(artist), Some(row)) = (
                item.item().and_downcast::<Artist>(),
                item.child().and_downcast::<LioListRow>(),
            ) {
                row.set_text(artist.name(), "");
                row.set_cover(artist.cover_art_id());
                row.set_data(&artist);
            }
        });
        imp.artists_list.set_factory(Some(&artist_factory));
        imp.artists_list
            .set_model(Some(&gtk::NoSelection::new(Some(artists.clone()))));
        imp.artists_list.connect_activate(|list, pos| {
            let artist = list
                .model()
                .and_then(|m| m.item(pos))
                .and_downcast::<Artist>();
            if let (Some(artist), Some(win)) = (artist, LioApplication::default().window()) {
                win.push_page(&LioArtistPage::new(&artist));
            }
        });

        imp.tracks_list
            .set_factory(Some(&track_list_row::factory()));
        imp.tracks_list
            .set_model(Some(&gtk::NoSelection::new(Some(tracks.clone()))));
        imp.tracks_list.connect_activate(glib::clone!(
            #[weak]
            tracks,
            move |_, pos| {
                let Some(account) = LioApplication::default().session().account() else {
                    return;
                };
                let rows = account
                    .db
                    .read(|c| queries::track_rows(c, &tracks.ids()))
                    .unwrap_or_default();
                LioApplication::default().playback().play_tracks(
                    rows,
                    pos as usize,
                    QueueOrigin::None,
                );
            }
        ));

        if let Some(win) = app.window() {
            win.bind_property("narrow", &imp.switcher_bar.get(), "reveal")
                .sync_create()
                .build();
        }
        let toggle = crate::widgets::download_toggle::new(
            lio_store::downloads::PinKind::Favourites,
            "favourites",
            "favourites",
        );
        imp.header_bar.pack_end(&toggle);
        imp.models.replace(Some((albums, artists, tracks)));
        let handler = app.library().connect_changed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.reload()
        ));
        imp.handler.replace(Some(handler));
        self.reload();
    }

    pub fn reload(&self) {
        let imp = self.imp();
        let Some((albums, artists, tracks)) = imp.models.borrow().clone() else {
            return;
        };
        let Some(account) = LioApplication::default().session().account() else {
            return;
        };
        let (album_ids, artist_ids, track_ids) = account
            .db
            .read(|c| {
                Ok((
                    queries::album_ids(c, AlbumSort::Name, &AlbumFilter::Starred, None)?,
                    queries::artist_ids(c, true)?,
                    queries::starred_track_ids(c)?,
                ))
            })
            .unwrap_or_default();
        let show = |stack: &gtk::Stack, empty: bool| {
            stack.set_visible_child_name(if empty { "empty" } else { "list" })
        };
        show(&imp.albums_stack, album_ids.is_empty());
        show(&imp.artists_stack, artist_ids.is_empty());
        show(&imp.tracks_stack, track_ids.is_empty());
        albums.set_ids(album_ids);
        artists.set_ids(artist_ids);
        tracks.set_ids(track_ids);
    }
}

impl Default for LioFavouritesPage {
    fn default() -> Self {
        Self::new()
    }
}
