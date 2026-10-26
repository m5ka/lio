//! Main window: sign-in / library stack, sidebar navigation, toasts, banner, player bar slot.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{CompositeTemplate, gio, glib};

use crate::application::LioApplication;

/// Library sections reachable from the sidebar (and `win.go-*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Home,
    Albums,
    Artists,
    Genres,
    Favourites,
    Downloads,
}

impl Section {
    pub const ALL: [Self; 6] = [
        Self::Home,
        Self::Albums,
        Self::Artists,
        Self::Genres,
        Self::Favourites,
        Self::Downloads,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Albums => "albums",
            Self::Artists => "artists",
            Self::Genres => "genres",
            Self::Favourites => "favourites",
            Self::Downloads => "downloads",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// Size of playlist covers in the sidebar.
const SIDEBAR_COVER: i32 = 24;

/// Widget name of the sidebar's "New Playlist" row.
const NEW_PLAYLIST_ROW: &str = "new-playlist";

/// A sidebar row: icon and label, like the sections in the template.
fn sidebar_row(icon: &str, label: &str) -> gtk::ListBoxRow {
    sidebar_row_with(&gtk::Image::from_icon_name(icon), label)
}

/// A playlist's sidebar row: its cover as a small thumbnail (the list icon until it
/// loads, or when the playlist has no cover).
fn playlist_row(playlist: &crate::models::Playlist) -> gtk::ListBoxRow {
    let stack = gtk::Stack::new();
    stack.add_named(
        &gtk::Image::from_icon_name("view-list-symbolic"),
        Some("placeholder"),
    );
    // An Image (not a Picture) so the cover is scaled to the icon size.
    let picture = gtk::Image::builder().pixel_size(SIDEBAR_COVER).build();
    stack.add_named(&picture, Some("cover"));
    let thumbnail = adw::Bin::builder()
        .child(&stack)
        .width_request(SIDEBAR_COVER)
        .height_request(SIDEBAR_COVER)
        .valign(gtk::Align::Center)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["sidebar-cover"])
        .build();
    // Rows are rebuilt on every change, so no load needs cancelling.
    let slot = std::cell::RefCell::default();
    crate::widgets::cover_view::show_cover(
        &stack,
        &picture,
        playlist.row().cover_art_id.as_deref(),
        crate::covers::SIZE_ROW,
        &slot,
    );
    sidebar_row_with(&thumbnail, playlist.name())
}

fn sidebar_row_with(icon: &impl IsA<gtk::Widget>, label: &str) -> gtk::ListBoxRow {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.append(icon);
    content.append(
        &gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    gtk::ListBoxRow::builder().child(&content).build()
}

/// Builds the root page of a sidebar section.
type SectionBuilder = Box<dyn Fn(Section) -> adw::NavigationPage>;

mod imp {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Default, CompositeTemplate, glib::Properties)]
    #[template(resource = "/dev/m5ka/Lio/ui/window.ui")]
    #[properties(wrapper_type = super::LioWindow)]
    pub struct LioWindow {
        /// Narrow layout (≤ 600 sp): collapsed sidebars, bottom view switchers.
        #[property(get)]
        pub narrow: Cell<bool>,
        /// Compact layout (≤ 400 sp): minimal player bar.
        #[property(get)]
        pub compact: Cell<bool>,
        #[template_child]
        pub narrow_breakpoint: TemplateChild<adw::Breakpoint>,
        #[template_child]
        pub compact_breakpoint: TemplateChild<adw::Breakpoint>,
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub main_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub sign_in_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub split_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub queue_split: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub queue_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub sidebar_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub nav_view: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub player_bar_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub player_bar_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub sync_spinner: TemplateChild<adw::Spinner>,
        #[template_child]
        pub now_playing_bin: TemplateChild<adw::Bin>,
        pub section: Cell<Option<Section>>,
        /// Playlist shown as the root page (picked in the sidebar), instead of a section.
        pub root_playlist: RefCell<Option<String>>,
        /// Sidebar rows below the divider, one per playlist.
        pub playlist_rows: RefCell<Vec<(gtk::ListBoxRow, crate::models::Playlist)>>,
        pub new_playlist_row: RefCell<Option<gtk::ListBoxRow>>,
        pub item_actions: RefCell<Option<gio::SimpleActionGroup>>,
        pub section_builder: RefCell<Option<SectionBuilder>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioWindow {
        const NAME: &'static str = "LioWindow";
        type Type = super::LioWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for LioWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            if crate::config::is_devel() {
                obj.add_css_class("devel");
            }
            obj.setup_sidebar();
            obj.setup_breakpoints();
            obj.setup_banner();
        }
    }

    impl WidgetImpl for LioWindow {}

    impl WindowImpl for LioWindow {
        fn close_request(&self) -> glib::Propagation {
            let obj = self.obj();
            obj.save_window_state();
            // While music plays the app keeps running (it holds itself, see
            // `background.rs`); the window is destroyed rather than hidden so its
            // renderer and GPU memory are released. Activating the app
            // builds a new window around the same session and queue.
            if let Some(app) = obj.lio_app()
                && app.should_hide_on_close()
            {
                tracing::debug!("window closed; playing in the background");
            }
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for LioWindow {}
    impl AdwApplicationWindowImpl for LioWindow {}
}

glib::wrapper! {
    pub struct LioWindow(ObjectSubclass<imp::LioWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl LioWindow {
    pub fn new(app: &LioApplication) -> Self {
        let obj: Self = glib::Object::builder().property("application", app).build();
        obj.restore_window_state();
        obj
    }

    fn lio_app(&self) -> Option<LioApplication> {
        self.application().and_downcast()
    }

    fn restore_window_state(&self) {
        let Some(app) = self.lio_app() else { return };
        let settings = app.settings();
        self.set_default_size(settings.int("window-width"), settings.int("window-height"));
        if settings.boolean("window-maximized") {
            self.maximize();
        }
    }

    fn save_window_state(&self) {
        let Some(app) = self.lio_app() else { return };
        let settings = app.settings();
        let (width, height) = self.default_size();
        let _ = settings.set_int("window-width", width);
        let _ = settings.set_int("window-height", height);
        let _ = settings.set_boolean("window-maximized", self.is_maximized());
    }

    fn setup_banner(&self) {
        self.imp().banner.connect_button_clicked(|_| {
            let _ = crate::application::LioApplication::default()
                .settings()
                .set_boolean("offline-mode", false);
        });
    }

    fn setup_breakpoints(&self) {
        let imp = self.imp();
        let set_narrow = |win: &Self, narrow: bool| {
            if win.imp().narrow.replace(narrow) != narrow {
                win.notify_narrow();
            }
        };
        let set_compact = |win: &Self, compact: bool| {
            if win.imp().compact.replace(compact) != compact {
                win.notify_compact();
                if let Some(bar) = win
                    .imp()
                    .player_bar_bin
                    .child()
                    .and_downcast::<crate::widgets::LioPlayerBar>()
                {
                    bar.set_compact(compact);
                }
            }
        };
        imp.narrow_breakpoint.connect_apply(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| set_narrow(&win, true)
        ));
        imp.narrow_breakpoint.connect_unapply(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| set_narrow(&win, false)
        ));
        // The compact breakpoint wins over the narrow one, so it implies narrow too.
        imp.compact_breakpoint.connect_apply(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                set_narrow(&win, true);
                set_compact(&win, true);
            }
        ));
        imp.compact_breakpoint.connect_unapply(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                set_compact(&win, false);
                set_narrow(&win, win.width() as f64 <= 600.0 * 1.0);
            }
        ));
    }

    fn setup_sidebar(&self) {
        let imp = self.imp();
        imp.sidebar_list.connect_row_activated(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, row| {
                if let Some(section) = Section::from_name(&row.widget_name()) {
                    win.show_section(section);
                } else if row.widget_name() == NEW_PLAYLIST_ROW {
                    glib::spawn_future_local(crate::widgets::pages::library_pages::new_playlist());
                } else if let Some(playlist) = win.sidebar_playlist(row) {
                    win.show_playlist(&playlist);
                }
            }
        ));
        // A divider between the sections and the playlists.
        imp.sidebar_list.set_header_func(|row, before| {
            let first_below = Section::from_name(&row.widget_name()).is_none()
                && before.is_some_and(|b| Section::from_name(&b.widget_name()).is_some());
            if first_below {
                if row.header().is_none() {
                    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
                    separator.set_margin_top(6);
                    separator.set_margin_bottom(6);
                    row.set_header(Some(&separator));
                }
            } else {
                row.set_header(None::<&gtk::Widget>);
            }
        });
    }

    fn sidebar_playlist(&self, row: &gtk::ListBoxRow) -> Option<crate::models::Playlist> {
        self.imp()
            .playlist_rows
            .borrow()
            .iter()
            .find(|(r, _)| r == row)
            .map(|(_, p)| p.clone())
    }

    /// Rebuilds the playlist rows below the divider from the library mirror.
    pub fn refresh_playlists(&self) {
        let imp = self.imp();
        let Some(account) = LioApplication::default().session().account() else {
            return;
        };
        let playlists: Vec<crate::models::Playlist> = account
            .db
            .read(lio_store::queries::playlists)
            .unwrap_or_default()
            .into_iter()
            .map(crate::models::Playlist::new)
            .collect();
        for (row, _) in imp.playlist_rows.take() {
            imp.sidebar_list.remove(&row);
        }
        let new_row = imp
            .new_playlist_row
            .borrow_mut()
            .get_or_insert_with(|| {
                let row = sidebar_row("list-add-symbolic", &gettext("New Playlist"));
                row.set_widget_name(NEW_PLAYLIST_ROW);
                // Opens a dialog; the current page stays selected.
                row.set_selectable(false);
                imp.sidebar_list.append(&row);
                row
            })
            .clone();
        let mut rows = Vec::with_capacity(playlists.len());
        for playlist in playlists {
            let row = playlist_row(&playlist);
            row.set_widget_name("playlist");
            row.set_tooltip_text(Some(playlist.name()));
            let id = playlist.id().to_owned();
            crate::widgets::item_menu::attach(&row, move |_| {
                Some(crate::widgets::item_menu::Target::Playlist(id.clone()))
            });
            imp.sidebar_list.insert(&row, new_row.index());
            rows.push((row, playlist));
        }
        imp.playlist_rows.replace(rows);
        imp.sidebar_list.invalidate_headers();

        // Keep the open playlist selected; a deleted one falls back to Home.
        let root = imp.root_playlist.borrow().clone();
        if let Some(id) = root {
            match imp
                .playlist_rows
                .borrow()
                .iter()
                .find(|(_, p)| p.id() == id)
            {
                Some((row, _)) => imp.sidebar_list.select_row(Some(row)),
                None => {
                    imp.root_playlist.replace(None);
                    self.show_section(Section::Home);
                }
            }
        }
    }

    /// Opens `playlist` as the root page, like a section.
    pub fn show_playlist(&self, playlist: &crate::models::Playlist) {
        let imp = self.imp();
        let row = imp
            .playlist_rows
            .borrow()
            .iter()
            .find(|(_, p)| p.id() == playlist.id())
            .map(|(r, _)| r.clone());
        if let Some(row) = row {
            imp.sidebar_list.select_row(Some(&row));
        }
        imp.nav_view
            .replace(&[crate::widgets::LioPlaylistPage::new(playlist).upcast()]);
        imp.section.set(None);
        imp.root_playlist.replace(Some(playlist.id().to_owned()));
        imp.split_view.set_show_content(true);
    }

    /// Installs the function that builds the root page for each sidebar section.
    pub fn set_section_builder(&self, f: impl Fn(Section) -> adw::NavigationPage + 'static) {
        self.imp().section_builder.replace(Some(Box::new(f)));
    }

    /// Replaces the content navigation stack with `section`'s root page.
    pub fn show_section(&self, section: Section) {
        let imp = self.imp();
        if let Some(row) = (0..)
            .map_while(|i| imp.sidebar_list.row_at_index(i))
            .find(|r| r.widget_name() == section.name())
        {
            imp.sidebar_list.select_row(Some(&row));
        }
        let page = imp
            .section_builder
            .borrow()
            .as_ref()
            .map(|build| build(section));
        if let Some(page) = page {
            imp.nav_view.replace(&[page]);
            imp.section.set(Some(section));
            imp.root_playlist.replace(None);
        }
        imp.split_view.set_show_content(true);
    }

    pub fn current_section(&self) -> Option<Section> {
        self.imp().section.get()
    }

    pub fn push_page(&self, page: &impl IsA<adw::NavigationPage>) {
        self.imp().nav_view.push(page);
    }

    pub fn nav_view(&self) -> adw::NavigationView {
        self.imp().nav_view.get()
    }

    pub fn show_loading(&self) {
        self.imp().main_stack.set_visible_child_name("loading");
    }

    pub fn show_sign_in(&self, page: &impl IsA<gtk::Widget>) {
        let imp = self.imp();
        imp.sign_in_bin.set_child(Some(page));
        imp.main_stack.set_visible_child_name("sign-in");
        imp.player_bar_revealer.set_reveal_child(false);
    }

    pub fn show_library(&self) {
        let imp = self.imp();
        imp.main_stack.set_visible_child_name("library");
        imp.sign_in_bin.set_child(None::<&gtk::Widget>);
        self.refresh_playlists();
        if imp.section.get().is_none() && imp.root_playlist.borrow().is_none() {
            self.show_section(Section::Home);
            let ms = crate::runtime::process_start().elapsed().as_millis() as u64;
            tracing::info!(startup_ms = ms, "library visible");
        }
    }

    /// Replaces the window's content (sidebars, pages and player bar) with the
    /// full-window Now Playing screen. Only from the library, and only while a track
    /// is loaded.
    pub fn show_now_playing(&self) {
        let imp = self.imp();
        let playback = LioApplication::default().playback();
        if imp.main_stack.visible_child_name().as_deref() != Some("library")
            || playback.current_track().is_none()
        {
            return;
        }
        if imp.now_playing_bin.child().is_none() {
            let screen = crate::widgets::LioNowPlaying::new(&playback);
            imp.now_playing_bin.set_child(Some(&screen));
        }
        imp.main_stack
            .set_visible_child_full("now-playing", gtk::StackTransitionType::SlideUp);
        if let Some(screen) = imp.now_playing_bin.child() {
            screen.grab_focus();
        }
    }

    pub fn hide_now_playing(&self) {
        let imp = self.imp();
        if self.is_now_playing_shown() {
            // Fullscreen belongs to Now Playing; the library goes back to a normal window.
            self.unfullscreen();
            imp.main_stack
                .set_visible_child_full("library", gtk::StackTransitionType::SlideDown);
        }
    }

    pub fn is_now_playing_shown(&self) -> bool {
        self.imp().main_stack.visible_child_name().as_deref() == Some("now-playing")
    }

    pub fn sign_in_page(&self) -> Option<crate::widgets::LioSignInPage> {
        self.imp().sign_in_bin.child().and_downcast()
    }

    /// Forgets the library pages (sign-out).
    pub fn reset_library(&self) {
        let imp = self.imp();
        imp.nav_view.replace(&[]);
        imp.section.set(None);
        imp.root_playlist.replace(None);
        for (row, _) in imp.playlist_rows.take() {
            imp.sidebar_list.remove(&row);
        }
        if let Some(row) = imp.new_playlist_row.take() {
            imp.sidebar_list.remove(&row);
        }
        imp.now_playing_bin.set_child(None::<&gtk::Widget>);
    }

    pub fn add_toast(&self, toast: adw::Toast) {
        self.imp().toast_overlay.add_toast(toast);
    }

    pub fn toast(&self, message: &str) {
        self.add_toast(adw::Toast::new(message));
    }

    pub fn toast_overlay(&self) -> adw::ToastOverlay {
        self.imp().toast_overlay.get()
    }

    pub fn banner(&self) -> adw::Banner {
        self.imp().banner.get()
    }

    pub fn set_player_bar(&self, bar: &impl IsA<gtk::Widget>) {
        self.imp().player_bar_bin.set_child(Some(bar));
    }

    pub fn set_queue_sidebar(&self, sidebar: &impl IsA<gtk::Widget>) {
        self.imp().queue_bin.set_child(Some(sidebar));
    }

    pub fn queue_split(&self) -> adw::OverlaySplitView {
        self.imp().queue_split.get()
    }

    pub fn set_item_actions(&self, group: gio::SimpleActionGroup) {
        self.imp().item_actions.replace(Some(group));
    }

    /// The `item.*` action group (see `actions::items`).
    pub fn item_actions(&self) -> Option<gio::SimpleActionGroup> {
        self.imp().item_actions.borrow().clone()
    }

    pub fn set_syncing(&self, syncing: bool) {
        self.imp().sync_spinner.set_visible(syncing);
    }

    /// Opens (or focuses) the search page.
    pub fn show_search(&self) {
        let imp = self.imp();
        if let Some(page) = imp.nav_view.find_page("search") {
            imp.nav_view.pop_to_page(&page);
            page.grab_focus();
            return;
        }
        let page = crate::widgets::LioSearchPage::new();
        imp.nav_view.push(&page);
        imp.split_view.set_show_content(true);
    }

    pub fn set_player_bar_visible(&self, visible: bool) {
        self.imp().player_bar_revealer.set_reveal_child(visible);
    }
}
