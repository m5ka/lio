//! Album grid page backed by [`IdListModel`]: the Albums section, genre and
//! favourite views, and the Home page's "See all".

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{CompositeTemplate, gio, glib};
use lio_store::queries::{self, AlbumFilter, AlbumSort};

use crate::application::LioApplication;
use crate::models::{Album, IdListModel, ItemKind};
use crate::settings::AlbumsSort;
use crate::widgets::{LioAlbumPage, LioAlbumTile, item_menu};

/// Sort entries shown in the menu, with their labels.
fn sort_entries() -> [(AlbumSort, &'static str, String); 6] {
    [
        (
            AlbumSort::RecentlyAdded,
            "recently-added",
            gettext("Recently Added"),
        ),
        (AlbumSort::Name, "name", gettext("Name")),
        (AlbumSort::Artist, "artist", gettext("Artist")),
        (AlbumSort::Year, "year", gettext("Year")),
        (AlbumSort::MostPlayed, "most-played", gettext("Most Played")),
        (AlbumSort::Random, "random", gettext("Random")),
    ]
}

fn sort_from_nick(nick: &str) -> AlbumSort {
    sort_entries()
        .into_iter()
        .find(|(_, n, _)| *n == nick)
        .map(|(s, _, _)| s)
        .unwrap_or_default()
}

fn nick_for(sort: AlbumSort) -> &'static str {
    sort_entries()
        .into_iter()
        .find(|(s, _, _)| *s == sort)
        .map(|(_, n, _)| n)
        .unwrap_or("recently-added")
}

pub fn sort_from_setting(sort: AlbumsSort) -> AlbumSort {
    sort_from_nick(sort.nick())
}

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/album_grid.ui")]
    pub struct LioAlbumGrid {
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub empty_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub sort_button: TemplateChild<gtk::MenuButton>,
        pub model: RefCell<Option<IdListModel>>,
        pub filter: RefCell<AlbumFilter>,
        pub sort: Cell<AlbumSort>,
        /// Persist the sort in `albums-sort` (the main Albums section only).
        pub persist_sort: Cell<bool>,
        pub handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioAlbumGrid {
        const NAME: &'static str = "LioAlbumGrid";
        type Type = super::LioAlbumGrid;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioAlbumGrid {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup_grid();
        }

        fn dispose(&self) {
            for (obj, id) in self.handlers.take() {
                obj.disconnect(id);
            }
        }
    }

    impl WidgetImpl for LioAlbumGrid {}
    impl NavigationPageImpl for LioAlbumGrid {}
}

glib::wrapper! {
    pub struct LioAlbumGrid(ObjectSubclass<imp::LioAlbumGrid>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioAlbumGrid {
    /// The main Albums section (sort persisted in settings).
    pub fn albums() -> Self {
        let app = LioApplication::default();
        let obj = Self::new(
            &gettext("Albums"),
            AlbumFilter::All,
            sort_from_setting(app.settings().albums_sort()),
        );
        obj.set_tag(Some("albums"));
        obj.imp().persist_sort.set(true);
        obj
    }

    pub fn new(title: &str, filter: AlbumFilter, sort: AlbumSort) -> Self {
        let obj: Self = glib::Object::builder().property("title", title).build();
        let imp = obj.imp();
        imp.filter.replace(filter);
        imp.sort.set(sort);
        obj.setup_sort_menu();
        let app = LioApplication::default();
        if let Some(account) = app.session().account() {
            let model = IdListModel::new(ItemKind::Album, &account.db);
            imp.grid_view
                .set_model(Some(&gtk::NoSelection::new(Some(model.clone()))));
            imp.model.replace(Some(model));
        }
        let library = app.library();
        let id = library.connect_changed(glib::clone!(
            #[weak]
            obj,
            move |_| obj.reload()
        ));
        imp.handlers
            .borrow_mut()
            .push((library.clone().upcast(), id));
        let id = library.connect_syncing_notify(glib::clone!(
            #[weak]
            obj,
            move |_| obj.update_stack()
        ));
        imp.handlers.borrow_mut().push((library.upcast(), id));
        obj.reload();
        obj
    }

    fn setup_grid(&self) {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let tile = LioAlbumTile::new();
            item_menu::attach(&tile, |tile: &LioAlbumTile| {
                tile.album()
                    .map(|a| item_menu::Target::Album(a.id().to_owned(), a.row().artist_id.clone()))
            });
            item.set_child(Some(&tile));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            if let (Some(album), Some(tile)) = (
                item.item().and_downcast::<Album>(),
                item.child().and_downcast::<LioAlbumTile>(),
            ) {
                tile.bind(&album);
            }
        });
        factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            if let Some(tile) = item.child().and_downcast::<LioAlbumTile>() {
                tile.unbind();
            }
        });
        let grid = &self.imp().grid_view;
        grid.set_factory(Some(&factory));
        grid.connect_activate(|grid, position| {
            let album = grid
                .model()
                .and_then(|m| m.item(position))
                .and_downcast::<Album>();
            if let (Some(album), Some(win)) = (album, LioApplication::default().window()) {
                win.push_page(&LioAlbumPage::new(&album));
            }
        });
    }

    fn setup_sort_menu(&self) {
        let imp = self.imp();
        let menu = gio::Menu::new();
        for (_, nick, label) in sort_entries() {
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(Some("grid.sort"), Some(&nick.to_variant()));
            menu.append_item(&item);
        }
        imp.sort_button.set_menu_model(Some(&menu));

        let action = gio::SimpleAction::new_stateful(
            "sort",
            Some(&String::static_variant_type()),
            &nick_for(imp.sort.get()).to_variant(),
        );
        action.connect_activate(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |action, value| {
                let Some(nick) = value.and_then(|v| v.get::<String>()) else {
                    return;
                };
                action.set_state(&nick.to_variant());
                page.imp().sort.set(sort_from_nick(&nick));
                if page.imp().persist_sort.get() {
                    let _ = LioApplication::default()
                        .settings()
                        .set_string("albums-sort", &nick);
                }
                page.reload();
            }
        ));
        let group = gio::SimpleActionGroup::new();
        group.add_action(&action);
        self.insert_action_group("grid", Some(&group));
    }

    /// Re-queries the album IDs (cheap) and refreshes the grid.
    pub fn reload(&self) {
        let imp = self.imp();
        let Some(model) = imp.model.borrow().clone() else {
            return;
        };
        let Some(account) = LioApplication::default().session().account() else {
            return;
        };
        let (sort, filter) = (imp.sort.get(), imp.filter.borrow().clone());
        let ids = account
            .db
            .read(|c| queries::album_ids(c, sort, &filter, None))
            .unwrap_or_default();
        model.set_ids(ids);
        self.update_stack();
    }

    fn update_stack(&self) {
        let imp = self.imp();
        let empty = imp.model.borrow().as_ref().is_none_or(|m| m.n_items() == 0);
        let syncing = LioApplication::default().library().syncing();
        let page = match (empty, syncing) {
            (false, _) => "grid",
            (true, true) => "syncing",
            (true, false) => "empty",
        };
        imp.stack.set_visible_child_name(page);
    }

    pub fn set_empty_text(&self, title: &str, description: Option<&str>) {
        let page = &self.imp().empty_page;
        page.set_title(title);
        page.set_description(description);
    }
}
