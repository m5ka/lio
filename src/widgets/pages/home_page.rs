//! Home page: Recently Added, Recently Played and Most Played album rows.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{CompositeTemplate, glib};
use lio_store::queries::{self, AlbumFilter, AlbumSort};

use crate::application::LioApplication;
use crate::models::Album;
use crate::widgets::{LioAlbumGrid, LioAlbumPage, LioAlbumTile, item_menu};

/// Albums per row.
const ROW_SIZE: usize = 20;

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/home_page.ui")]
    pub struct LioHomePage {
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub sections_box: TemplateChild<gtk::Box>,
        pub handlers: RefCell<Vec<glib::SignalHandlerId>>,
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

impl LioHomePage {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn reload(&self) {
        let imp = self.imp();
        let app = LioApplication::default();
        let Some(account) = app.session().account() else {
            return;
        };
        while let Some(child) = imp.sections_box.first_child() {
            imp.sections_box.remove(&child);
        }
        let sections = [
            (
                gettext("Recently Added"),
                AlbumSort::RecentlyAdded,
                AlbumFilter::All,
            ),
            (
                gettext("Recently Played"),
                AlbumSort::RecentlyPlayed,
                AlbumFilter::RecentlyPlayed,
            ),
            (
                gettext("Most Played"),
                AlbumSort::MostPlayed,
                AlbumFilter::RecentlyPlayed,
            ),
        ];
        let mut any = false;
        for (title, sort, filter) in sections {
            let rows = account
                .db
                .read(|c| {
                    let ids = queries::album_ids(c, sort, &filter, Some(ROW_SIZE))?;
                    queries::album_rows(c, &ids)
                })
                .unwrap_or_default();
            if rows.is_empty() {
                continue;
            }
            any = true;
            imp.sections_box
                .append(&section(&title, sort, filter, rows));
        }
        let page = match (any, app.library().syncing()) {
            (true, _) => "content",
            (false, true) => "syncing",
            (false, false) => "empty",
        };
        imp.stack.set_visible_child_name(page);
    }
}

impl Default for LioHomePage {
    fn default() -> Self {
        Self::new()
    }
}

fn section(
    title: &str,
    sort: AlbumSort,
    filter: AlbumFilter,
    rows: Vec<lio_store::AlbumRow>,
) -> gtk::Widget {
    let header = gtk::Box::builder()
        .spacing(12)
        .margin_start(18)
        .margin_end(18)
        .build();
    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["title-3"])
        .build();
    let see_all = gtk::Button::builder()
        .label(gettext("See All"))
        .css_classes(["flat"])
        .build();
    let title_owned = title.to_owned();
    see_all.connect_clicked(move |_| {
        if let Some(win) = LioApplication::default().window() {
            win.push_page(&LioAlbumGrid::new(&title_owned, filter.clone(), sort));
        }
    });
    header.append(&label);
    header.append(&see_all);

    let row = gtk::Box::builder()
        .spacing(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    for album in rows {
        let tile = LioAlbumTile::new();
        tile.bind(&Album::new(album));
        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak]
            tile,
            move |_, _, _, _| open(&tile)
        ));
        tile.add_controller(click);
        tile.set_focusable(true);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            tile,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if matches!(
                    key,
                    gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::space
                ) {
                    open(&tile);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        ));
        tile.add_controller(keys);
        item_menu::attach(&tile, |tile: &LioAlbumTile| {
            tile.album()
                .map(|a| item_menu::Target::Album(a.id().to_owned(), a.row().artist_id.clone()))
        });
        tile.add_css_class("home-tile");
        row.append(&tile);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&row)
        .build();

    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    section.append(&header);
    section.append(&scroller);
    section.upcast()
}

fn open(tile: &LioAlbumTile) {
    if let (Some(album), Some(win)) = (tile.album(), LioApplication::default().window()) {
        win.push_page(&LioAlbumPage::new(&album));
    }
}
