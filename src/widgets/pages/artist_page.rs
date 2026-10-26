//! Artist page: header with Play All / Shuffle and the artist's albums.

use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::ngettext;
use gtk::{CompositeTemplate, glib};
use lio_store::queries::{self, AlbumFilter, AlbumSort};

use crate::actions::items;
use crate::application::LioApplication;
use crate::models::{Album, Artist};
use crate::playback_controller::QueueOrigin;
use crate::runtime;
use crate::widgets::{LioAlbumPage, LioAlbumTile, item_menu};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/artist_page.ui")]
    pub struct LioArtistPage {
        #[template_child]
        pub header_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub name_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub meta_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub albums_box: TemplateChild<gtk::FlowBox>,
        pub artist: RefCell<Option<Artist>>,
        pub handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioArtistPage {
        const NAME: &'static str = "LioArtistPage";
        type Type = super::LioArtistPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.install_action_async("artist.play", None, |page, _, _| async move {
                page.play(false).await
            });
            klass.install_action_async("artist.shuffle", None, |page, _, _| async move {
                page.play(true).await
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioArtistPage {
        fn dispose(&self) {
            if let Some(id) = self.handler.take() {
                LioApplication::default().library().disconnect(id);
            }
        }
    }

    impl WidgetImpl for LioArtistPage {}
    impl NavigationPageImpl for LioArtistPage {}
}

glib::wrapper! {
    pub struct LioArtistPage(ObjectSubclass<imp::LioArtistPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioArtistPage {
    pub fn new(artist: &Artist) -> Self {
        let obj: Self = glib::Object::builder()
            .property("title", artist.name())
            .build();
        let imp = obj.imp();
        crate::widgets::adaptive::stack_when_narrow(&imp.header_box);
        imp.artist.replace(Some(artist.clone()));
        imp.name_label.set_label(artist.name());
        imp.avatar.set_text(Some(artist.name()));
        if let (Some(id), Some(covers)) = (
            artist.cover_art_id().map(str::to_owned),
            LioApplication::default().covers(),
        ) {
            glib::spawn_future_local(glib::clone!(
                #[weak]
                obj,
                async move {
                    if let Some(t) = covers.load(&id, crate::covers::SIZE_LARGE, None).await {
                        obj.imp().avatar.set_custom_image(Some(&t));
                    }
                }
            ));
        }
        imp.albums_box.connect_child_activated(|_, child| {
            let album = child
                .child()
                .and_downcast::<LioAlbumTile>()
                .and_then(|t| t.album());
            if let (Some(album), Some(win)) = (album, LioApplication::default().window()) {
                win.push_page(&LioAlbumPage::new(&album));
            }
        });
        let handler = LioApplication::default()
            .library()
            .connect_changed(glib::clone!(
                #[weak]
                obj,
                move |_| obj.show_albums()
            ));
        imp.handler.replace(Some(handler));
        obj.show_albums();
        obj.refresh_from_server();
        obj
    }

    fn show_albums(&self) {
        let imp = self.imp();
        let Some(artist) = imp.artist.borrow().clone() else {
            return;
        };
        let Some(account) = LioApplication::default().session().account() else {
            return;
        };
        let rowid = artist.rowid();
        let rows = account
            .db
            .read(|c| {
                let ids =
                    queries::album_ids(c, AlbumSort::Year, &AlbumFilter::Artist(rowid), None)?;
                queries::album_rows(c, &ids)
            })
            .unwrap_or_default();
        imp.meta_label.set_label(
            &ngettext("{} album", "{} albums", rows.len() as u32)
                .replace("{}", &rows.len().to_string()),
        );
        imp.albums_box.remove_all();
        for row in rows {
            let tile = LioAlbumTile::new();
            tile.bind(&Album::new(row));
            item_menu::attach(&tile, |tile: &LioAlbumTile| {
                tile.album()
                    .map(|a| item_menu::Target::Album(a.id().to_owned(), a.row().artist_id.clone()))
            });
            imp.albums_box.append(&tile);
        }
    }

    /// Mirrors the artist's album list from the server (albums may be missing locally).
    fn refresh_from_server(&self) {
        let session = LioApplication::default().session();
        let (Some(account), true) = (session.account(), session.is_online()) else {
            return;
        };
        let Some(id) = self
            .imp()
            .artist
            .borrow()
            .as_ref()
            .map(|a| a.id().to_owned())
        else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let result = runtime::run(async move {
                    let artist = account.client.get_artist(&id).await?;
                    account
                        .db
                        .run(move |c| {
                            lio_store::library::upsert_artist(c, &artist)?;
                            lio_store::library::upsert_albums(c, &artist.album).map(drop)
                        })
                        .await
                        .map_err(|e| lio_subsonic::Error::Decode(e.to_string()))
                })
                .await;
                if result.is_ok() {
                    page.show_albums();
                }
            }
        ));
    }

    async fn play(&self, shuffle: bool) {
        let Some(id) = self
            .imp()
            .artist
            .borrow()
            .as_ref()
            .map(|a| a.id().to_owned())
        else {
            return;
        };
        let app = LioApplication::default();
        match items::resolve_tracks("artist", &id).await {
            Ok(tracks) if !tracks.is_empty() => {
                let origin = QueueOrigin::Artist(id);
                if shuffle {
                    app.playback().shuffle_tracks(tracks, origin);
                } else {
                    app.playback().play_tracks(tracks, 0, origin);
                }
            }
            Ok(_) => {}
            Err(e) => {
                if let Some(win) = app.window() {
                    win.toast(&e);
                }
            }
        }
    }
}
