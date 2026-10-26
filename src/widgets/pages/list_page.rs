//! Generic list page (artists, genres, playlists) and its row widget.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, gio, glib};

type BindFn = Rc<dyn Fn(&LioListRow, &glib::Object)>;
type ActivateFn = Rc<dyn Fn(&glib::Object)>;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/list_page.ui")]
    pub struct LioListPage {
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub empty_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub list_view: TemplateChild<gtk::ListView>,
        pub model: RefCell<Option<gio::ListModel>>,
        pub handler: RefCell<Option<(glib::Object, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioListPage {
        const NAME: &'static str = "LioListPage";
        type Type = super::LioListPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioListPage {
        fn dispose(&self) {
            if let Some((obj, id)) = self.handler.take() {
                obj.disconnect(id);
            }
        }
    }

    impl WidgetImpl for LioListPage {}
    impl NavigationPageImpl for LioListPage {}
}

glib::wrapper! {
    pub struct LioListPage(ObjectSubclass<imp::LioListPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioListPage {
    pub fn new(title: &str, tag: &str) -> Self {
        glib::Object::builder()
            .property("title", title)
            .property("tag", tag)
            .build()
    }

    /// Adds a button to the end of the header bar.
    pub fn add_header_button(&self, icon: &str, tooltip: &str, on_click: impl Fn() + 'static) {
        let button = gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tooltip)
            .build();
        button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
        button.connect_clicked(move |_| on_click());
        self.imp().header_bar.pack_end(&button);
    }

    pub fn set_empty(&self, icon: &str, title: &str) {
        let page = &self.imp().empty_page;
        page.set_icon_name(Some(icon));
        page.set_title(title);
    }

    /// Shows `model`, binding rows with `bind` and opening items with `activate`.
    pub fn setup(
        &self,
        model: &impl IsA<gio::ListModel>,
        bind: impl Fn(&LioListRow, &glib::Object) + 'static,
        activate: impl Fn(&glib::Object) + 'static,
    ) {
        let bind: BindFn = Rc::new(bind);
        let activate: ActivateFn = Rc::new(activate);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            item.set_child(Some(&LioListRow::new()));
        });
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            if let (Some(obj), Some(row)) = (item.item(), item.child().and_downcast::<LioListRow>())
            {
                row.reset();
                bind(&row, &obj);
            }
        });
        factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            if let Some(row) = item.child().and_downcast::<LioListRow>() {
                row.reset();
            }
        });
        let imp = self.imp();
        let model: gio::ListModel = model.clone().upcast();
        imp.list_view.set_factory(Some(&factory));
        imp.list_view
            .set_model(Some(&gtk::NoSelection::new(Some(model.clone()))));
        imp.list_view.connect_activate(move |view, position| {
            if let Some(obj) = view.model().and_then(|m| m.item(position)) {
                activate(&obj);
            }
        });
        let id = model.connect_items_changed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_, _, _, _| page.update_stack()
        ));
        imp.handler.replace(Some((model.clone().upcast(), id)));
        imp.model.replace(Some(model));
        self.update_stack();
    }

    fn update_stack(&self) {
        let imp = self.imp();
        let empty = imp.model.borrow().as_ref().is_none_or(|m| m.n_items() == 0);
        imp.stack
            .set_visible_child_name(if empty { "empty" } else { "list" });
    }
}

mod row_imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/list_row.ui")]
    pub struct LioListRow {
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub subtitle_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub offline_icon: TemplateChild<gtk::Image>,
        pub cancellable: RefCell<Option<gio::Cancellable>>,
        pub data: RefCell<Option<glib::Object>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioListRow {
        const NAME: &'static str = "LioListRow";
        type Type = super::LioListRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioListRow {}
    impl WidgetImpl for LioListRow {}
    impl BoxImpl for LioListRow {}
}

glib::wrapper! {
    pub struct LioListRow(ObjectSubclass<row_imp::LioListRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl LioListRow {
    pub fn new() -> Self {
        glib::Object::new()
    }

    fn reset(&self) {
        let imp = self.imp();
        if let Some(c) = imp.cancellable.take() {
            c.cancel();
        }
        imp.avatar.set_custom_image(None::<&gtk::gdk::Paintable>);
        imp.offline_icon.set_visible(false);
        imp.data.replace(None);
    }

    /// The object currently shown (for context menus).
    pub fn data(&self) -> Option<glib::Object> {
        self.imp().data.borrow().clone()
    }

    pub fn set_data(&self, obj: &impl IsA<glib::Object>) {
        self.imp().data.replace(Some(obj.clone().upcast()));
    }

    pub fn set_text(&self, title: &str, subtitle: &str) {
        let imp = self.imp();
        imp.title_label.set_label(title);
        imp.subtitle_label.set_label(subtitle);
        imp.subtitle_label.set_visible(!subtitle.is_empty());
        imp.avatar.set_text(Some(title));
        self.update_property(&[gtk::accessible::Property::Label(&format!(
            "{title}, {subtitle}"
        ))]);
    }

    pub fn set_icon(&self, icon: &str) {
        let avatar = &self.imp().avatar;
        avatar.set_show_initials(false);
        avatar.set_icon_name(Some(icon));
    }

    pub fn set_offline(&self, offline: bool) {
        self.imp().offline_icon.set_visible(offline);
    }

    /// Loads a cover into the avatar.
    pub fn set_cover(&self, cover_id: Option<&str>) {
        let imp = self.imp();
        imp.avatar.set_show_initials(true);
        let (Some(id), Some(covers)) = (
            cover_id,
            crate::application::LioApplication::default().covers(),
        ) else {
            return;
        };
        let cancellable = gio::Cancellable::new();
        imp.cancellable.replace(Some(cancellable.clone()));
        let id = id.to_owned();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = row)]
            self,
            async move {
                if let Some(texture) = covers
                    .load(&id, crate::covers::SIZE_ROW, Some(&cancellable))
                    .await
                {
                    row.imp().avatar.set_custom_image(Some(&texture));
                }
            }
        ));
    }
}

impl Default for LioListRow {
    fn default() -> Self {
        Self::new()
    }
}
