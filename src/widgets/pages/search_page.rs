//! Search page: as-you-type search over artists, albums and songs.

use std::cell::Cell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::ngettext;
use gtk::{CompositeTemplate, glib};
use lio_store::search::{self, SearchResults};

use crate::application::LioApplication;
use crate::models::{Album, Artist, Track};
use crate::playback_controller::QueueOrigin;
use crate::runtime;
use crate::widgets::{LioAlbumPage, LioAlbumTile, LioArtistPage, LioTrackRow, item_menu};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/search_page.ui")]
    pub struct LioSearchPage {
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub artists_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub artists_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub albums_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub albums_box: TemplateChild<gtk::FlowBox>,
        #[template_child]
        pub tracks_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub tracks_list: TemplateChild<gtk::ListBox>,
        /// Increments per query; stale results are dropped.
        pub generation: Cell<u64>,
        pub tracks: std::cell::RefCell<Vec<lio_store::TrackRow>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioSearchPage {
        const NAME: &'static str = "LioSearchPage";
        type Type = super::LioSearchPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioSearchPage {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.search_entry.connect_search_changed(glib::clone!(
                #[weak]
                obj,
                move |entry| obj.search(entry.text().to_string())
            ));
            self.search_entry
                .connect_stop_search(|entry| entry.set_text(""));
            self.albums_box.connect_child_activated(|_, child| {
                let album = child
                    .child()
                    .and_downcast::<LioAlbumTile>()
                    .and_then(|t| t.album());
                if let (Some(album), Some(win)) = (album, LioApplication::default().window()) {
                    win.push_page(&LioAlbumPage::new(&album));
                }
            });
            self.tracks_list.connect_row_activated(glib::clone!(
                #[weak]
                obj,
                move |_, row| {
                    let tracks = obj.imp().tracks.borrow().clone();
                    let index = row.index().max(0) as usize;
                    LioApplication::default().playback().play_tracks(
                        tracks,
                        index,
                        QueueOrigin::Search,
                    );
                }
            ));
        }
    }

    impl WidgetImpl for LioSearchPage {
        fn grab_focus(&self) -> bool {
            self.search_entry.grab_focus()
        }
    }

    impl NavigationPageImpl for LioSearchPage {
        fn shown(&self) {
            self.parent_shown();
            self.search_entry.grab_focus();
        }
    }
}

glib::wrapper! {
    pub struct LioSearchPage(ObjectSubclass<imp::LioSearchPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioSearchPage {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_query(&self, query: &str) {
        self.imp().search_entry.set_text(query);
    }

    fn search(&self, text: String) {
        let imp = self.imp();
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);
        if text.trim().is_empty() {
            imp.stack.set_visible_child_name("prompt");
            return;
        }
        let session = LioApplication::default().session();
        let Some(account) = session.account() else {
            return;
        };
        let client = session.is_online().then(|| account.client.clone());
        let started = std::time::Instant::now();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let db = account.db.clone();
                let query = text.clone();
                let result =
                    runtime::run(async move { search::search(&db, client.as_ref(), &query).await })
                        .await;
                if page.imp().generation.get() != generation {
                    return; // A newer query is running.
                }
                tracing::debug!(ms = started.elapsed().as_millis() as u64, query = %text, "search");
                match result {
                    Ok(results) => page.show(results),
                    Err(e) => tracing::warn!("search failed: {e}"),
                }
            }
        ));
    }

    fn show(&self, results: SearchResults) {
        let imp = self.imp();
        if results.is_empty() {
            imp.stack.set_visible_child_name("none");
            return;
        }
        imp.artists_list.remove_all();
        for row in &results.artists {
            let artist = Artist::new(row.clone());
            let action_row = adw::ActionRow::builder()
                .title(artist.name())
                .subtitle(
                    ngettext("{} album", "{} albums", row.album_count.max(0) as u32)
                        .replace("{}", &row.album_count.to_string()),
                )
                .activatable(true)
                .build();
            let avatar = adw::Avatar::new(32, Some(artist.name()), true);
            action_row.add_prefix(&avatar);
            action_row.connect_activated(glib::clone!(
                #[strong]
                artist,
                move |_| {
                    if let Some(win) = LioApplication::default().window() {
                        win.push_page(&LioArtistPage::new(&artist));
                    }
                }
            ));
            let id = artist.id().to_owned();
            item_menu::attach(&action_row, move |_| {
                Some(item_menu::Target::Artist(id.clone()))
            });
            imp.artists_list.append(&action_row);
        }
        imp.artists_label.set_visible(!results.artists.is_empty());
        imp.artists_list.set_visible(!results.artists.is_empty());

        imp.albums_box.remove_all();
        for row in &results.albums {
            let tile = LioAlbumTile::new();
            tile.bind(&Album::new(row.clone()));
            item_menu::attach(&tile, |tile: &LioAlbumTile| {
                tile.album()
                    .map(|a| item_menu::Target::Album(a.id().to_owned(), a.row().artist_id.clone()))
            });
            imp.albums_box.append(&tile);
        }
        imp.albums_label.set_visible(!results.albums.is_empty());
        imp.albums_box.set_visible(!results.albums.is_empty());

        imp.tracks_list.remove_all();
        for row in &results.tracks {
            let track = Track::new(row.clone());
            let widget = LioTrackRow::new(&track, None);
            if let Some(album) = track.album() {
                widget.set_subtitle(&format!("{} — {album}", track.artist()));
            }
            item_menu::attach(&widget, |row: &LioTrackRow| {
                row.track().map(|t| item_menu::Target::Track {
                    id: t.id().to_owned(),
                    album_id: t.row().album_id.clone(),
                })
            });
            imp.tracks_list.append(&widget);
        }
        imp.tracks.replace(results.tracks.clone());
        imp.tracks_label.set_visible(!results.tracks.is_empty());
        imp.tracks_list.set_visible(!results.tracks.is_empty());
        imp.stack.set_visible_child_name("results");
    }
}

impl Default for LioSearchPage {
    fn default() -> Self {
        Self::new()
    }
}
