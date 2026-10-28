//! Home page: a Library Mix panel and the latest addition (both tinted from their
//! covers), Recently Played and Recently Added as lists side by side, and a random Dig
//! Deeper row.

use std::collections::HashMap;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{CompositeTemplate, glib, graphene, gsk};
use lio_store::AlbumRow;
use lio_store::queries::{self, AlbumFilter, AlbumSort};

use crate::application::LioApplication;
use crate::covers::{SIZE_GRID, SIZE_ROW};
use crate::models::{Album, format_duration};
use crate::playback_controller::QueueOrigin;
use crate::widgets::tint::{self, Tint};
use crate::widgets::{LioAlbumGrid, LioAlbumPage, cover_view, item_menu};

/// Albums in Recently Played, and in Recently Added (after the one in Just Added).
const LIST_SIZE: usize = 6;
/// Albums queued by Play Mix; the first three are fanned out on the panel.
const MIX_SIZE: usize = 10;
/// Albums in Dig Deeper.
const DEEPER_SIZE: usize = 10;
/// Cover edges in pixels.
const FAN_COVER: i32 = 150;
const JUST_ADDED_COVER: i32 = 120;
const CARD_COVER: i32 = 160;

mod imp {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/home_page.ui")]
    pub struct LioHomePage {
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub sections_box: TemplateChild<gtk::Box>,
        pub handlers: RefCell<Vec<glib::SignalHandlerId>>,
        /// Bumped by each reload and Library Mix reshuffle, so a newer one cancels an
        /// older one still looking up its cover colours.
        pub generation: Cell<u64>,
        pub mix_shuffle: Cell<u64>,
        /// The next Library Mix, drawn ahead with its panel's colour already looked up,
        /// so its reshuffle button can show it at once.
        pub next_mix: RefCell<Option<Vec<AlbumRow>>>,
        /// Where the random sections go, so Reshuffle can replace just them.
        pub mix_slot: RefCell<Option<gtk::Stack>>,
        pub deeper_slot: RefCell<Option<gtk::Stack>>,
        /// Albums in the library, for the Library Mix description.
        pub count: Cell<i64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioHomePage {
        const NAME: &'static str = "LioHomePage";
        type Type = super::LioHomePage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioHomePage {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            let library = LioApplication::default().library();
            let reload = glib::clone!(
                #[weak]
                obj,
                move |_: &crate::library_sync::LioLibrary| obj.reload()
            );
            self.handlers
                .borrow_mut()
                .push(library.connect_changed(reload.clone()));
            self.handlers
                .borrow_mut()
                .push(library.connect_syncing_notify(reload));
            obj.reload();
        }

        fn dispose(&self) {
            let library = LioApplication::default().library();
            for id in self.handlers.take() {
                library.disconnect(id);
            }
        }
    }

    impl WidgetImpl for LioHomePage {}
    impl NavigationPageImpl for LioHomePage {}
}

glib::wrapper! {
    pub struct LioHomePage(ObjectSubclass<imp::LioHomePage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// What the page shows, read from the library mirror in one go.
#[derive(Default)]
struct HomeData {
    count: i64,
    added: Vec<AlbumRow>,
    played: Vec<AlbumRow>,
}

impl LioHomePage {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Rebuilds the page, drawing new random picks too. While the page is showing, the
    /// old content stays until the new covers' colours are known, so the new panels
    /// start in their colours; the first time, it is built at once and the colours fade
    /// in.
    pub fn reload(&self) {
        let imp = self.imp();
        let app = LioApplication::default();
        let Some(account) = app.session().account() else {
            return;
        };
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);
        let data = account
            .db
            .read(|c| {
                let rows = |sort, filter, limit| {
                    queries::album_ids(c, sort, &filter, Some(limit))
                        .and_then(|ids| queries::album_rows(c, &ids))
                };
                Ok(HomeData {
                    count: c
                        .query_row("SELECT COUNT(*) FROM album", [], |r| r.get(0))
                        .unwrap_or(0),
                    added: rows(AlbumSort::RecentlyAdded, AlbumFilter::All, LIST_SIZE + 1)?,
                    played: rows(
                        AlbumSort::RecentlyPlayed,
                        AlbumFilter::RecentlyPlayed,
                        LIST_SIZE,
                    )?,
                })
            })
            .unwrap_or_default();
        if data.count == 0 || data.added.is_empty() {
            while let Some(child) = imp.sections_box.first_child() {
                imp.sections_box.remove(&child);
            }
            let page = if app.library().syncing() {
                "syncing"
            } else {
                "empty"
            };
            imp.stack.set_visible_child_name(page);
            return;
        }
        let (mix, deeper) = (
            self.random_albums(MIX_SIZE),
            self.random_albums(DEEPER_SIZE),
        );
        if imp.stack.visible_child_name().as_deref() != Some("content") {
            self.build(data, mix, deeper, None);
            return;
        }
        let ids = data
            .added
            .iter()
            .chain(mix.first())
            .chain(&deeper)
            .map(|a| a.cover_art_id.clone())
            .collect::<Vec<_>>();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let accents = accents(ids).await;
                if page.imp().generation.get() == generation {
                    page.build(data, mix, deeper, Some(&accents));
                }
            }
        ));
    }

    fn build(
        &self,
        data: HomeData,
        mix: Vec<AlbumRow>,
        deeper: Vec<AlbumRow>,
        accents: Option<&Accents>,
    ) {
        let imp = self.imp();
        while let Some(child) = imp.sections_box.first_child() {
            imp.sections_box.remove(&child);
        }
        let HomeData {
            count,
            mut added,
            played,
        } = data;
        imp.count.set(count);
        let latest = added.remove(0);

        let top = gtk::Box::builder()
            .spacing(16)
            .margin_start(18)
            .margin_end(18)
            .build();
        let mix_slot = slot();
        top.append(&mix_slot);
        top.append(&self.just_added(latest, accents));
        follow_narrow(&top);
        imp.sections_box.append(&top);

        let lists = gtk::Box::builder()
            .spacing(24)
            .homogeneous(true)
            .margin_start(18)
            .margin_end(18)
            .build();
        if !played.is_empty() {
            lists.append(&played_section(played));
        }
        if !added.is_empty() {
            lists.append(&added_section(added, accents));
        }
        follow_narrow(&lists);
        if lists.first_child().is_some() {
            imp.sections_box.append(&lists);
        }

        let deeper_slot = slot();
        imp.sections_box.append(&deeper_slot);
        imp.mix_slot.replace(Some(mix_slot));
        imp.deeper_slot.replace(Some(deeper_slot));
        self.fill_mix(mix, accents);
        self.fill_deeper(deeper, accents);
        self.prepare_mix();
        imp.stack.set_visible_child_name("content");
    }

    /// `limit` random albums. Library Mix and Dig Deeper each draw their own, so small
    /// libraries fill both.
    fn random_albums(&self, limit: usize) -> Vec<AlbumRow> {
        let Some(account) = LioApplication::default().session().account() else {
            return Vec::new();
        };
        account
            .db
            .read(|c| {
                let ids = queries::album_ids(c, AlbumSort::Random, &AlbumFilter::All, Some(limit))?;
                queries::album_rows(c, &ids)
            })
            .unwrap_or_default()
    }

    /// Shows a new Library Mix, leaving the rest of the page as it is. The panel waits
    /// for its colour (its button's accent can't fade); with the mix drawn ahead by
    /// [`Self::prepare_mix`] that is already known, so it crossfades in at once.
    fn reshuffle_mix(&self) {
        let imp = self.imp();
        let generation = imp.generation.get();
        let shuffle = imp.mix_shuffle.get() + 1;
        imp.mix_shuffle.set(shuffle);
        let mix = imp
            .next_mix
            .take()
            .unwrap_or_else(|| self.random_albums(MIX_SIZE));
        let ids = vec![mix.first().and_then(|a| a.cover_art_id.clone())];
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let accents = accents(ids).await;
                let imp = page.imp();
                if imp.generation.get() == generation && imp.mix_shuffle.get() == shuffle {
                    page.fill_mix(mix, Some(&accents));
                    page.prepare_mix();
                }
            }
        ));
    }

    /// Draws the next Library Mix and looks up its panel's colour.
    fn prepare_mix(&self) {
        let mix = self.random_albums(MIX_SIZE);
        let id = mix.first().and_then(|a| a.cover_art_id.clone());
        let generation = self.imp().generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                if let Some(id) = id {
                    tint::cover_accent(&id).await;
                }
                if page.imp().generation.get() == generation {
                    page.imp().next_mix.replace(Some(mix));
                }
            }
        ));
    }

    /// Shows new Dig Deeper picks at once; the cards fade into their colours.
    fn reshuffle_deeper(&self) {
        self.fill_deeper(self.random_albums(DEEPER_SIZE), None);
    }

    fn fill_mix(&self, mix: Vec<AlbumRow>, accents: Option<&Accents>) {
        let imp = self.imp();
        if let Some(slot) = imp.mix_slot.borrow().as_ref() {
            swap(slot, Some(&self.mix_panel(mix, imp.count.get(), accents)));
        }
    }

    fn fill_deeper(&self, deeper: Vec<AlbumRow>, accents: Option<&Accents>) {
        if let Some(slot) = self.imp().deeper_slot.borrow().as_ref() {
            let section = (!deeper.is_empty()).then(|| self.deeper_section(deeper, accents));
            swap(slot, section.as_ref());
        }
    }

    /// The Library Mix panel: a few random covers fanned out, tinted by the front one.
    fn mix_panel(&self, mix: Vec<AlbumRow>, count: i64, accents: Option<&Accents>) -> gtk::Widget {
        let panel = gtk::Box::builder()
            .spacing(24)
            .hexpand(true)
            .css_classes(["card", "home-panel"])
            .build();

        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .hexpand(true)
            .build();
        text.append(&badge(&gettext("Library Mix")));
        text.append(
            &gtk::Label::builder()
                .label(gettext("Shuffle Your Library"))
                .xalign(0.0)
                .wrap(true)
                .css_classes(["title-1"])
                .build(),
        );
        let mut description = ngettext(
            "Whole albums, picked at random from {count} album.",
            "Whole albums, picked at random from {count} albums.",
            count.clamp(0, u32::MAX.into()) as u32,
        )
        .replace("{count}", &count.to_string());
        if let Some(first) = mix.first() {
            description.push(' ');
            description.push_str(&gettext("Starts with {album}.").replace("{album}", &first.name));
        }
        text.append(
            &gtk::Label::builder()
                .label(description)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label"])
                .build(),
        );

        let ids: Vec<String> = mix.iter().map(|a| a.id.clone()).collect();
        let play = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("media-playback-start-symbolic")
                    .label(gettext("Play Mix"))
                    .build(),
            )
            .css_classes(["pill", "suggested-action"])
            .build();
        play.connect_clicked(move |_| play_mix(ids.clone()));
        let reshuffle = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(gettext("Reshuffle"))
            .valign(gtk::Align::Center)
            .css_classes(["circular", "home-reshuffle"])
            .build();
        reshuffle.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.reshuffle_mix()
        ));
        let buttons = gtk::Box::builder()
            .spacing(10)
            .margin_top(12)
            .vexpand(true)
            .valign(gtk::Align::End)
            .build();
        buttons.append(&play);
        buttons.append(&reshuffle);
        text.append(&buttons);
        panel.append(&text);

        // Back to front: right, left, then the album the mix starts with.
        let fan = gtk::Fixed::builder()
            .width_request(FAN_COVER * 2)
            .height_request(FAN_COVER + 50)
            .valign(gtk::Align::Center)
            .build();
        let spots = [(2, 50.0, 9.0), (1, -50.0, -8.0), (0, 0.0, 0.0)];
        for (index, dx, angle) in spots {
            let Some(album) = mix.get(index) else {
                continue;
            };
            let cover = cover(
                album.cover_art_id.as_deref(),
                FAN_COVER,
                SIZE_GRID,
                "album-cover",
            );
            cover.add_css_class("home-fan-cover");
            fan.put(&cover, 0.0, 0.0);
            let half = FAN_COVER as f32 / 2.0;
            let transform = gsk::Transform::new()
                .translate(&graphene::Point::new(FAN_COVER as f32 + dx, half + 25.0))
                .rotate(angle)
                .translate(&graphene::Point::new(-half, -half));
            fan.set_child_transform(&cover, Some(&transform));
        }
        if let Some(win) = LioApplication::default().window() {
            win.bind_property("narrow", &fan, "visible")
                .invert_boolean()
                .sync_create()
                .build();
        }
        panel.append(&fan);

        if let Some(front) = mix.first() {
            tint(&panel, front.cover_art_id.as_deref(), panel_css, accents);
        }
        panel.upcast()
    }

    /// The most recently added album, tinted from its cover.
    fn just_added(&self, album: AlbumRow, accents: Option<&Accents>) -> gtk::Widget {
        let panel = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .width_request(300)
            .css_classes(["card", "home-panel"])
            .build();
        tint(&panel, album.cover_art_id.as_deref(), panel_css, accents);
        let album = Album::new(album);

        let open_button = gtk::Button::builder()
            .child(&cover(
                album.cover_art_id(),
                JUST_ADDED_COVER,
                SIZE_GRID,
                "album-cover",
            ))
            .tooltip_text(gettext("Open Album"))
            .css_classes(["flat", "home-cover-button"])
            .build();
        open_button.connect_clicked(glib::clone!(
            #[strong]
            album,
            move |_| open(&album)
        ));
        let head = gtk::Box::builder().spacing(14).margin_bottom(8).build();
        head.append(&open_button);
        let added = badge(&gettext("Just Added"));
        added.set_valign(gtk::Align::End);
        head.append(&added);
        panel.append(&head);

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
        for (text, class) in [
            (album.name().to_owned(), "title-3"),
            (album.artist(), "heading"),
            (meta.join(" · "), "caption"),
        ] {
            let label = gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes([class])
                .build();
            if class == "caption" {
                label.add_css_class("dim-label");
            }
            panel.append(&label);
        }

        let play = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("media-playback-start-symbolic")
                    .label(gettext("Play"))
                    .build(),
            )
            .action_name("item.play")
            .action_target(&("album", album.id()).to_variant())
            .halign(gtk::Align::Start)
            .vexpand(true)
            .valign(gtk::Align::End)
            .margin_top(12)
            .css_classes(["pill", "suggested-action"])
            .build();
        panel.append(&play);
        attach_menu(&panel, album.row());
        panel.upcast()
    }

    /// Random albums as cards whose caption is tinted from the cover above it.
    fn deeper_section(&self, albums: Vec<AlbumRow>, accents: Option<&Accents>) -> gtk::Widget {
        let reshuffle = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("view-refresh-symbolic")
                    .label(gettext("Reshuffle"))
                    .build(),
            )
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        reshuffle.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.reshuffle_deeper()
        ));
        let header = header(
            &gettext("Dig Deeper"),
            Some(&gettext("Random albums from your library")),
            &reshuffle,
        );
        header.set_margin_start(18);
        header.set_margin_end(18);

        let row = gtk::Box::builder()
            .spacing(14)
            .margin_start(18)
            .margin_end(18)
            .margin_bottom(6)
            .build();
        for album in albums {
            let album = Album::new(album);
            let card = gtk::Button::builder()
                .overflow(gtk::Overflow::Hidden)
                .tooltip_text(album.name())
                .css_classes(["card", "home-card"])
                .build();
            let content = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            content.append(&cover(
                album.cover_art_id(),
                CARD_COVER,
                SIZE_GRID,
                "home-card-cover",
            ));
            let caption = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .margin_top(10)
                .margin_bottom(12)
                .margin_start(12)
                .margin_end(12)
                .build();
            caption.append(
                &gtk::Label::builder()
                    .label(album.name())
                    .xalign(0.0)
                    .max_width_chars(1)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["heading"])
                    .build(),
            );
            let mut byline = album.artist();
            if let Some(year) = album.row().year {
                byline = format!("{byline} · {year}");
            }
            caption.append(
                &gtk::Label::builder()
                    .label(byline)
                    .xalign(0.0)
                    .max_width_chars(1)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["caption", "dim-label"])
                    .build(),
            );
            content.append(&caption);
            card.set_child(Some(&content));
            card.update_property(&[gtk::accessible::Property::Label(&format!(
                "{}, {}",
                album.name(),
                album.artist()
            ))]);
            tint(&card, album.cover_art_id(), card_css, accents);
            attach_menu(&card, album.row());
            card.connect_clicked(move |_| open(&album));
            row.append(&card);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&row)
            .build();

        let section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        section.append(&header);
        section.append(&scroller);
        section.upcast()
    }
}

impl Default for LioHomePage {
    fn default() -> Self {
        Self::new()
    }
}

fn played_section(albums: Vec<AlbumRow>) -> gtk::Widget {
    let list = album_list(albums, |a| a.played_at, |_, _, _| {});
    let see_all = see_all(
        &gettext("Recently Played"),
        AlbumFilter::RecentlyPlayed,
        AlbumSort::RecentlyPlayed,
    );
    titled(&header(&gettext("Recently Played"), None, &see_all), &list)
}

/// A list rather than a grid, so it lines up with Recently Played beside it. Each row is
/// a whole new album, so its cover sits in a swatch of its colour and the row says how
/// many songs it has.
fn added_section(albums: Vec<AlbumRow>, accents: Option<&Accents>) -> gtk::Widget {
    let list = album_list(
        albums,
        |a| Some(a.created_at).filter(|&at| at > 0),
        |swatch, details, album| {
            tint(swatch, album.cover_art_id(), swatch_css, accents);
            let songs = album.row().song_count;
            if songs > 0 {
                details.append(
                    &gtk::Label::builder()
                        .label(
                            ngettext("{} song", "{} songs", songs as u32)
                                .replace("{}", &songs.to_string()),
                        )
                        .css_classes(["caption", "dim-label"])
                        .build(),
                );
            }
        },
    );
    let see_all = see_all(
        &gettext("Recently Added"),
        AlbumFilter::All,
        AlbumSort::RecentlyAdded,
    );
    titled(&header(&gettext("Recently Added"), None, &see_all), &list)
}

/// Albums as rows in two parts: the cover in a swatch with even padding round it, then
/// title and artist, anything `extra` adds (it gets the swatch and the box the details
/// go in), the day of `when` (played or added) and a play button.
fn album_list(
    albums: Vec<AlbumRow>,
    when: fn(&AlbumRow) -> Option<i64>,
    extra: impl Fn(&adw::Bin, &gtk::Box, &Album),
) -> gtk::ListBox {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .valign(gtk::Align::Start)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["boxed-list", "home-album-list"])
        .build();
    let albums: Vec<Album> = albums.into_iter().map(Album::new).collect();
    for album in &albums {
        let swatch = adw::Bin::builder()
            .child(&cover(album.cover_art_id(), 40, SIZE_ROW, "cover-small"))
            .css_classes(["home-row-swatch"])
            .build();
        let titles = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        for (text, classes) in [
            (album.name().to_owned(), &[][..]),
            (album.artist(), &["caption", "dim-label"][..]),
        ] {
            titles.append(
                &gtk::Label::builder()
                    .label(text)
                    .xalign(0.0)
                    .max_width_chars(1)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(classes.to_vec())
                    .build(),
            );
        }
        let details = gtk::Box::builder()
            .spacing(12)
            .margin_start(12)
            .margin_end(6)
            .hexpand(true)
            .build();
        details.append(&titles);
        extra(&swatch, &details, album);
        if let Some(day) = when(album.row()).map(relative_day) {
            details.append(
                &gtk::Label::builder()
                    .label(day)
                    .css_classes(["caption", "dim-label"])
                    .build(),
            );
        }
        details.append(
            &gtk::Button::builder()
                .icon_name("media-playback-start-symbolic")
                .tooltip_text(gettext("Play"))
                .action_name("item.play")
                .action_target(&("album", album.id()).to_variant())
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular", "play-icon"])
                .build(),
        );
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.append(&swatch);
        content.append(&details);
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(true)
            .build();
        row.update_property(&[gtk::accessible::Property::Label(&format!(
            "{}, {}",
            album.name(),
            album.artist()
        ))]);
        attach_menu(&row, album.row());
        list.append(&row);
    }
    list.connect_row_activated(move |_, row| {
        if let Some(album) = usize::try_from(row.index())
            .ok()
            .and_then(|i| albums.get(i))
        {
            open(album);
        }
    });
    list
}

/// A section heading with an optional subtitle and a button at the end.
fn header(title: &str, subtitle: Option<&str>, button: &gtk::Button) -> gtk::Box {
    let titles = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    titles.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["title-2"])
            .build(),
    );
    if let Some(subtitle) = subtitle {
        titles.append(
            &gtk::Label::builder()
                .label(subtitle)
                .xalign(0.0)
                .css_classes(["dim-label"])
                .build(),
        );
    }
    let header = gtk::Box::builder().spacing(12).build();
    header.append(&titles);
    header.append(button);
    header
}

fn titled(header: &gtk::Box, content: &impl IsA<gtk::Widget>) -> gtk::Widget {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    section.append(header);
    section.append(content);
    section.upcast()
}

fn see_all(title: &str, filter: AlbumFilter, sort: AlbumSort) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(gettext("See All"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let title = title.to_owned();
    button.connect_clicked(move |_| {
        if let Some(win) = LioApplication::default().window() {
            win.push_page(&LioAlbumGrid::new(&title, filter.clone(), sort));
        }
    });
    button
}

fn badge(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .css_classes(["caption-heading", "home-badge"])
        .build()
}

/// A cover of `size` pixels (loaded at `load`), with the disc placeholder until it arrives.
fn cover(cover_id: Option<&str>, size: i32, load: u32, class: &str) -> adw::Bin {
    let placeholder = gtk::Image::builder()
        .icon_name("media-optical-cd-audio-symbolic")
        .pixel_size((size / 2).max(16))
        .css_classes(["dim-label"])
        .build();
    let image = gtk::Image::builder().pixel_size(size).build();
    let stack = gtk::Stack::new();
    stack.add_named(&placeholder, Some("placeholder"));
    stack.add_named(&image, Some("cover"));
    cover_view::show_cover(&stack, &image, cover_id, load, &Default::default());
    adw::Bin::builder()
        .child(&stack)
        .width_request(size)
        .height_request(size)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .overflow(gtk::Overflow::Hidden)
        .css_classes([class])
        .build()
}

/// Holds a random section; a reshuffle crossfades the new one in (see [`swap`]).
fn slot() -> gtk::Stack {
    gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(500)
        .hhomogeneous(false)
        .vhomogeneous(false)
        .interpolate_size(true)
        .hexpand(true)
        .build()
}

/// Crossfades `slot` to `child` (or to nothing), dropping the old child once it has
/// faded out.
fn swap(slot: &gtk::Stack, child: Option<&gtk::Widget>) {
    let old = slot.visible_child();
    slot.set_visible(child.is_some());
    if let Some(child) = child {
        slot.add_child(child);
        slot.set_visible_child(child);
    }
    let Some(old) = old else {
        return;
    };
    let delay = std::time::Duration::from_millis(u64::from(slot.transition_duration()) + 50);
    glib::timeout_add_local_once(
        delay,
        glib::clone!(
            #[weak]
            slot,
            #[weak]
            old,
            move || {
                if old.parent().as_ref() == Some(slot.upcast_ref()) {
                    slot.remove(&old);
                }
            }
        ),
    );
}

/// Cover accents (by cover ID) looked up before building, so widgets start in their
/// colour instead of changing to it.
type Accents = HashMap<String, Option<(u8, u8, u8)>>;

/// Looks the covers up side by side; each may mean downloading the cover.
async fn accents(ids: Vec<Option<String>>) -> Accents {
    let ids: Vec<String> = ids.into_iter().flatten().collect();
    let colours = futures_util::future::join_all(ids.iter().map(|id| tint::cover_accent(id))).await;
    ids.into_iter().zip(colours).collect()
}

/// Tints `widget` from the vivid colour of its cover: at once when `accents` has it
/// (looked up ahead), else once it loads (the CSS fades it in). Greyscale covers leave
/// it untinted. The tint lasts as long as the widget, so one fading out keeps it.
fn tint(
    widget: &impl IsA<gtk::Widget>,
    cover_id: Option<&str>,
    css: fn((u8, u8, u8)) -> String,
    accents: Option<&Accents>,
) {
    let Some(id) = cover_id.filter(|id| !id.is_empty()).map(str::to_owned) else {
        return;
    };
    let widget = widget.clone().upcast::<gtk::Widget>();
    if let Some(known) = accents.and_then(|a| a.get(&id)) {
        if let Some(colour) = known {
            apply_tint(&widget, &css(*colour));
        }
        return;
    }
    glib::spawn_future_local(glib::clone!(
        #[weak]
        widget,
        async move {
            if let Some(colour) = tint::cover_accent(&id).await {
                apply_tint(&widget, &css(colour));
            }
        }
    ));
}

fn apply_tint(widget: &gtk::Widget, css: &str) {
    let tint = Tint::new(widget);
    tint.set(css);
    let tint = std::cell::RefCell::new(Some(tint));
    widget.connect_destroy(move |_| drop(tint.take()));
}

/// Lays `boxed` out vertically while the window is narrow.
fn follow_narrow(boxed: &gtk::Box) {
    if let Some(win) = LioApplication::default().window() {
        win.bind_property("narrow", boxed, "orientation")
            .transform_to(|_, narrow: bool| {
                Some(if narrow {
                    gtk::Orientation::Vertical
                } else {
                    gtk::Orientation::Horizontal
                })
            })
            .sync_create()
            .build();
    }
}

fn attach_menu(widget: &impl IsA<gtk::Widget>, album: &AlbumRow) {
    let target = item_menu::Target::Album(album.id.clone(), album.artist_id.clone());
    item_menu::attach(widget, move |_| Some(target.clone()));
}

/// A large panel: the cover's colour as the accent, over a gradient of darker shades of
/// it that keeps white text legible.
fn panel_css(colour: (u8, u8, u8)) -> String {
    let (r, g, b) = colour;
    format!(
        "{}
         background-image: linear-gradient(120deg,
           oklab(from rgb({r},{g},{b}) max(min(l, 0.46), 0.34) a b),
           oklab(from rgb({r},{g},{b}) max(min(l, 0.3), 0.22) a b));
         color: white;",
        tint::accent_declarations(colour)
    )
}

/// A Dig Deeper card: one flat shade of the cover's colour.
fn card_css((r, g, b): (u8, u8, u8)) -> String {
    format!(
        "background-image: linear-gradient(
           oklab(from rgb({r},{g},{b}) max(min(l, 0.4), 0.3) a b),
           oklab(from rgb({r},{g},{b}) max(min(l, 0.4), 0.3) a b));
         color: white;"
    )
}

/// The swatch round a Recently Added row's cover: one flat shade of the cover's colour.
fn swatch_css((r, g, b): (u8, u8, u8)) -> String {
    format!(
        "background-image: linear-gradient(
           oklab(from rgb({r},{g},{b}) max(min(l, 0.4), 0.3) a b),
           oklab(from rgb({r},{g},{b}) max(min(l, 0.4), 0.3) a b));"
    )
}

/// "Today", "Yesterday", a weekday within the last week, else the date.
fn relative_day(at: i64) -> String {
    let (Ok(then), Ok(now)) = (
        glib::DateTime::from_unix_local(at),
        glib::DateTime::now_local(),
    ) else {
        return String::new();
    };
    let midnight = now.to_unix()
        - i64::from(now.hour()) * 3600
        - i64::from(now.minute()) * 60
        - i64::from(now.second());
    if at >= midnight {
        gettext("Today")
    } else if at >= midnight - 86_400 {
        gettext("Yesterday")
    } else {
        let format = if at >= midnight - 6 * 86_400 {
            "%A"
        } else {
            "%e %b"
        };
        then.format(format)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    }
}

/// Plays the mix's albums in order: the first straight away, the rest queued behind it
/// as their tracks resolve. Albums with nothing playable (e.g. offline) are skipped.
fn play_mix(ids: Vec<String>) {
    glib::spawn_future_local(async move {
        let app = LioApplication::default();
        let mut started = false;
        for id in ids {
            match crate::actions::items::resolve_tracks("album", &id).await {
                Ok(tracks) if tracks.is_empty() => {}
                Ok(tracks) if started => app.playback().enqueue(tracks),
                Ok(tracks) => {
                    app.playback().play_tracks(tracks, 0, QueueOrigin::None);
                    started = true;
                }
                Err(e) if !started => {
                    if let Some(win) = app.window() {
                        win.toast(&e);
                    }
                    return;
                }
                Err(_) => {}
            }
        }
        if !started && let Some(win) = app.window() {
            win.toast(&gettext("Nothing to play"));
        }
    });
}

fn open(album: &Album) {
    if let Some(win) = LioApplication::default().window() {
        win.push_page(&LioAlbumPage::new(album));
    }
}
