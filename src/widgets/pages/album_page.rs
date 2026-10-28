//! Album page: header, Play/Shuffle, tracks grouped by disc.

use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{CompositeTemplate, gio, glib};
use lio_store::TrackRow;

use crate::application::LioApplication;
use crate::covers::SIZE_LARGE;
use crate::models::{Album, Track, format_duration};
use crate::playback_controller::QueueOrigin;
use crate::runtime;
use crate::widgets::{LioTrackRow, cover_view};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/album_page.ui")]
    pub struct LioAlbumPage {
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub error_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub header_box: TemplateChild<gtk::Box>,
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
        pub meta_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub track_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub star_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub download_button: TemplateChild<gtk::ToggleButton>,
        pub updating_download: std::cell::Cell<bool>,
        pub library_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub album: RefCell<Option<Album>>,
        pub tracks: RefCell<Vec<TrackRow>>,
        pub rows: RefCell<Vec<LioTrackRow>>,
        pub cancellable: RefCell<Option<gio::Cancellable>>,
        pub playback_handler: RefCell<Option<glib::SignalHandlerId>>,
        /// Background gradient from the cover's colour.
        pub tint: RefCell<Option<crate::widgets::tint::Tint>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioAlbumPage {
        const NAME: &'static str = "LioAlbumPage";
        type Type = super::LioAlbumPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.install_action("album.play", None, |page, _, _| page.play(false));
            klass.install_action("album.shuffle", None, |page, _, _| page.play(true));
            klass.install_action_async("album.star", None, |page, _, _| async move {
                let id = page
                    .imp()
                    .album
                    .borrow()
                    .as_ref()
                    .map(|a| a.id().to_owned());
                if let (Some(id), Some(win)) = (id, LioApplication::default().window()) {
                    crate::actions::annotation::toggle_star(&win, "album", &id).await;
                }
            });
            klass.install_action_async("album.go-artist", None, |page, _, _| async move {
                let artist = page
                    .imp()
                    .album
                    .borrow()
                    .as_ref()
                    .and_then(|a| a.row().artist_id.clone());
                if let (Some(id), Some(win)) = (artist, LioApplication::default().window()) {
                    crate::widgets::navigate::artist(&win, &id).await;
                }
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioAlbumPage {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.cover_bin.set_size_request(200, 200);
            crate::widgets::adaptive::stack_when_narrow(&self.header_box);
            self.download_button.connect_toggled(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    if obj.imp().updating_download.get() {
                        return;
                    }
                    let id = obj.imp().album.borrow().as_ref().map(|a| a.id().to_owned());
                    if let (Some(id), Some(win)) = (id, LioApplication::default().window()) {
                        let _ = gtk::prelude::WidgetExt::activate_action(
                            &win,
                            "item.download",
                            Some(&("album", id.as_str()).to_variant()),
                        );
                    }
                }
            ));
            let offline = LioApplication::default().offline();
            offline.connect_downloads_changed(glib::clone!(
                #[weak]
                obj,
                move |_, _| obj.update_download()
            ));
            self.track_list.set_header_func(|row, before| {
                let disc = |r: &gtk::ListBoxRow| {
                    r.downcast_ref::<LioTrackRow>()
                        .and_then(|r| r.track())
                        .and_then(|t| t.row().disc_no)
                };
                let this = disc(row);
                let prev = before.and_then(disc);
                if this.is_some() && this != prev && (before.is_some() || this != Some(1)) {
                    let label = gtk::Label::builder()
                        .label(gettext("Disc {}").replace("{}", &this.unwrap_or(1).to_string()))
                        .xalign(0.0)
                        .margin_start(12)
                        .margin_top(12)
                        .margin_bottom(6)
                        .css_classes(["heading"])
                        .build();
                    row.set_header(Some(&label));
                } else {
                    row.set_header(None::<&gtk::Widget>);
                }
            });
            self.track_list.connect_row_activated(glib::clone!(
                #[weak]
                obj,
                move |_, row| obj.play_from(row.index().max(0) as usize)
            ));
            let playback = LioApplication::default().playback();
            let handler = playback.connect_current_track_notify(glib::clone!(
                #[weak]
                obj,
                move |_| obj.update_playing()
            ));
            self.playback_handler.replace(Some(handler));
            let handler = LioApplication::default()
                .library()
                .connect_changed(glib::clone!(
                    #[weak]
                    obj,
                    move |_| obj.update_stars()
                ));
            self.library_handler.replace(Some(handler));
        }

        fn dispose(&self) {
            if let Some(id) = self.playback_handler.take() {
                LioApplication::default().playback().disconnect(id);
            }
            if let Some(id) = self.library_handler.take() {
                LioApplication::default().library().disconnect(id);
            }
            if let Some(c) = self.cancellable.take() {
                c.cancel();
            }
        }
    }

    impl WidgetImpl for LioAlbumPage {}
    impl NavigationPageImpl for LioAlbumPage {}
}

glib::wrapper! {
    pub struct LioAlbumPage(ObjectSubclass<imp::LioAlbumPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioAlbumPage {
    pub fn new(album: &Album) -> Self {
        let obj: Self = glib::Object::builder()
            .property("title", album.name())
            .build();
        obj.show_header(album);
        obj.imp().album.replace(Some(album.clone()));
        obj.load(album.id().to_owned());
        obj
    }

    fn show_header(&self, album: &Album) {
        let imp = self.imp();
        imp.title_label.set_label(album.name());
        imp.artist_label.set_label(&album.artist());
        self.action_set_enabled("album.go-artist", album.row().artist_id.is_some());
        let row = album.row();
        let mut meta = Vec::new();
        if let Some(year) = row.year {
            meta.push(year.to_string());
        }
        if row.song_count > 0 {
            meta.push(
                ngettext("{} track", "{} tracks", row.song_count as u32)
                    .replace("{}", &row.song_count.to_string()),
            );
        }
        if row.duration_s > 0 {
            meta.push(format_duration(row.duration_s));
        }
        imp.meta_label.set_label(&meta.join(" · "));
        cover_view::show_cover(
            &imp.cover_stack,
            &*imp.cover_picture,
            album.cover_art_id(),
            SIZE_LARGE,
            &imp.cancellable,
        );
        crate::widgets::tint::Tint::from_cover(
            self,
            album.cover_art_id(),
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |colour| page.apply_tint(colour)
            ),
        );
    }

    /// The page starts in a translucent-looking wash of the cover's colour (mixed into
    /// the background, so text keeps its contrast) and fades to the plain background.
    fn apply_tint(&self, (r, g, b): (u8, u8, u8)) {
        self.imp()
            .tint
            .borrow_mut()
            .get_or_insert_with(|| crate::widgets::tint::Tint::new(self))
            .set(&format!(
                "background-image: linear-gradient(to bottom,
                   color-mix(in srgb, rgb({r},{g},{b}) 34%, var(--window-bg-color)) 0px,
                   color-mix(in srgb, rgb({r},{g},{b}) 14%, var(--window-bg-color)) 260px,
                   var(--window-bg-color) 560px);"
            ));
    }

    fn load(&self, album_id: String) {
        let imp = self.imp();
        imp.stack.set_visible_child_name("loading");
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let app = LioApplication::default();
                let session = app.session();
                let Some(account) = session.account() else {
                    return;
                };
                let online = session.is_online();
                let id = album_id.clone();
                let result = runtime::run(async move {
                    if online {
                        match account.client.get_album(&id).await {
                            Ok(album) => {
                                return account
                                    .db
                                    .run(move |c| {
                                        let rowid = lio_store::library::upsert_album_with_tracks(
                                            c, &album,
                                        )?;
                                        let tracks =
                                            lio_store::queries::album_track_rows(c, rowid)?;
                                        Ok((lio_store::queries::album_row(c, rowid)?, tracks))
                                    })
                                    .await
                                    .map_err(|e| e.to_string());
                            }
                            Err(e) if e.is_connectivity() => {}
                            Err(e) => return Err(crate::widgets::error_toast::message(&e)),
                        }
                    }
                    // Offline (or server unreachable): use the local mirror.
                    account
                        .db
                        .run(move |c| {
                            let Some(album) = lio_store::queries::album_row_by_id(c, &id)? else {
                                return Ok((None, Vec::new()));
                            };
                            let tracks = lio_store::queries::album_track_rows(c, album.rowid)?;
                            Ok((Some(album), tracks))
                        })
                        .await
                        .map_err(|e| e.to_string())
                })
                .await;
                match result {
                    Ok((album_row, tracks)) if !tracks.is_empty() => {
                        if let Some(row) = album_row {
                            let album = Album::new(row);
                            page.show_header(&album);
                            page.imp().album.replace(Some(album));
                        }
                        page.show_tracks(tracks);
                    }
                    Ok(_) => page.show_error(&gettext("This album isn't available offline")),
                    Err(message) => page.show_error(&message),
                }
            }
        ));
    }

    fn show_error(&self, message: &str) {
        let imp = self.imp();
        imp.error_page.set_description(Some(message));
        imp.stack.set_visible_child_name("error");
    }

    fn show_tracks(&self, tracks: Vec<TrackRow>) {
        let imp = self.imp();
        while let Some(child) = imp.track_list.first_child() {
            imp.track_list.remove(&child);
        }
        let album_artist = imp
            .album
            .borrow()
            .as_ref()
            .and_then(|a| a.row().artist_display.clone());
        let mut rows = Vec::with_capacity(tracks.len());
        for row in &tracks {
            let track = Track::new(row.clone());
            let widget = LioTrackRow::new(&track, album_artist.as_deref());
            crate::widgets::item_menu::attach(&widget, |row: &LioTrackRow| {
                row.track()
                    .map(|t| crate::widgets::item_menu::Target::Track {
                        id: t.id().to_owned(),
                        album_id: t.row().album_id.clone(),
                    })
            });
            imp.track_list.append(&widget);
            rows.push(widget);
        }
        imp.rows.replace(rows);
        imp.tracks.replace(tracks);
        imp.stack.set_visible_child_name("content");
        self.update_playing();
        self.update_stars();
        self.update_download();
    }

    fn update_download(&self) {
        let imp = self.imp();
        let Some(id) = imp.album.borrow().as_ref().map(|a| a.id().to_owned()) else {
            return;
        };
        let app = LioApplication::default();
        let pin = app
            .offline()
            .pin_state(lio_store::downloads::PinKind::Album, &id);
        imp.updating_download.set(true);
        imp.download_button.set_active(pin.is_some());
        imp.updating_download.set(false);
        let can = app.session().account().is_some_and(|a| a.can_download);
        imp.download_button.set_visible(can);
        let tooltip = match &pin {
            Some(p) if p.available_offline() => gettext("Available Offline — Remove Download"),
            Some(_) => gettext("Downloading — Cancel"),
            None => gettext("Download"),
        };
        imp.download_button.set_tooltip_text(Some(&tooltip));
        imp.download_button.set_icon_name(match &pin {
            Some(p) if p.available_offline() => "folder-download-symbolic",
            Some(_) => "content-loading-symbolic",
            None => "folder-download-symbolic",
        });
        // Offline, songs without a local copy can't play.
        let online = app.session().is_online();
        for row in imp.rows.borrow().iter() {
            let available = row.track().is_some_and(|t| t.offline_available());
            row.set_sensitive(online || available);
        }
    }

    /// Refreshes star buttons from the mirror (after favourites change).
    fn update_stars(&self) {
        let imp = self.imp();
        let starred = imp
            .album
            .borrow()
            .as_ref()
            .is_some_and(|a| crate::actions::annotation::is_starred("album", a.id()));
        set_star_icon(&imp.star_button, starred);
        for row in imp.rows.borrow().iter() {
            if let Some(t) = row.track() {
                row.set_starred(crate::actions::annotation::is_starred("track", t.id()));
            }
        }
    }

    fn update_playing(&self) {
        let current = LioApplication::default()
            .playback()
            .current_track()
            .map(|t| t.rowid());
        for row in self.imp().rows.borrow().iter() {
            let is_current = row.track().is_some_and(|t| Some(t.rowid()) == current);
            row.set_playing(is_current);
        }
    }

    fn play_from(&self, index: usize) {
        let tracks = self.imp().tracks.borrow().clone();
        let origin = self.album_origin();
        LioApplication::default()
            .playback()
            .play_tracks(tracks, index, origin);
    }

    fn play(&self, shuffle: bool) {
        let tracks = self.imp().tracks.borrow().clone();
        let playback = LioApplication::default().playback();
        if shuffle {
            playback.shuffle_tracks(tracks, self.album_origin());
        } else {
            playback.play_tracks(tracks, 0, self.album_origin());
        }
    }

    fn album_origin(&self) -> QueueOrigin {
        self.imp()
            .album
            .borrow()
            .as_ref()
            .map(|a| QueueOrigin::Album(a.id().to_owned()))
            .unwrap_or_default()
    }
}

pub fn set_star_icon(button: &gtk::Button, starred: bool) {
    button.set_icon_name(if starred {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    let label = if starred {
        gettext("Remove from Favourites")
    } else {
        gettext("Add to Favourites")
    };
    button.set_tooltip_text(Some(&label));
    button.update_property(&[gtk::accessible::Property::Label(&label)]);
    if starred {
        button.add_css_class("accent");
    } else {
        button.remove_css_class("accent");
    }
}
