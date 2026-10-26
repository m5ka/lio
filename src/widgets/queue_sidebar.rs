//! "Up Next" sidebar: the play queue with drag-and-drop and keyboard reordering,
//! removal, Clear and Save as Playlist.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{CompositeTemplate, glib};

use crate::application::LioApplication;
use crate::playback_controller::PlaybackController;
use crate::runtime;
use crate::widgets::editable_rows;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/queue_sidebar.ui")]
    pub struct LioQueueSidebar {
        #[template_child]
        pub title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub queue_list: TemplateChild<gtk::ListView>,
        pub controller: RefCell<Option<PlaybackController>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioQueueSidebar {
        const NAME: &'static str = "LioQueueSidebar";
        type Type = super::LioQueueSidebar;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.install_action("queue.clear", None, |sidebar, _, _| {
                if let Some(c) = sidebar.controller() {
                    c.clear_queue();
                }
            });
            klass.install_action_async("queue.save", None, |sidebar, _, _| async move {
                sidebar.save_as_playlist().await;
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioQueueSidebar {}
    impl WidgetImpl for LioQueueSidebar {}
    impl BinImpl for LioQueueSidebar {}
}

glib::wrapper! {
    pub struct LioQueueSidebar(ObjectSubclass<imp::LioQueueSidebar>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioQueueSidebar {
    pub fn new(controller: &PlaybackController) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().controller.replace(Some(controller.clone()));
        obj.setup(controller);
        obj
    }

    fn controller(&self) -> Option<PlaybackController> {
        self.imp().controller.borrow().clone()
    }

    fn setup(&self, controller: &PlaybackController) {
        let imp = self.imp();
        let model = controller.queue_model();
        imp.queue_list
            .set_model(Some(&gtk::NoSelection::new(Some(model.clone()))));
        imp.queue_list.set_factory(Some(&row_factory(controller)));
        imp.queue_list.connect_activate(glib::clone!(
            #[weak]
            controller,
            move |_, pos| controller.play_position(pos as usize, None, false)
        ));
        model.connect_items_changed(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            move |m, _, _, _| sidebar.update(m.n_items())
        ));
        self.update(model.n_items());
    }

    fn update(&self, n: u32) {
        let imp = self.imp();
        imp.stack
            .set_visible_child_name(if n == 0 { "empty" } else { "list" });
        imp.title
            .set_subtitle(&ngettext("{} song", "{} songs", n).replace("{}", &n.to_string()));
        self.action_set_enabled("queue.clear", n > 0);
        self.action_set_enabled("queue.save", n > 0);
    }

    /// Asks for a name and creates a server playlist from the queue.
    async fn save_as_playlist(&self) {
        let Some(controller) = self.controller() else {
            return;
        };
        let app = LioApplication::default();
        let Some(account) = app.session().account() else {
            return;
        };
        let entry = gtk::Entry::builder()
            .placeholder_text(gettext("Playlist name"))
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Save Queue as Playlist"))
            .extra_child(&entry)
            .close_response("cancel")
            .default_response("save")
            .build();
        dialog.add_responses(&[("cancel", &gettext("_Cancel")), ("save", &gettext("_Save"))]);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_response_enabled("save", false);
        entry.connect_changed(glib::clone!(
            #[weak]
            dialog,
            move |e| dialog.set_response_enabled("save", !e.text().trim().is_empty())
        ));
        if dialog.choose_future(Some(self)).await != "save" {
            return;
        }
        let name = entry.text().trim().to_owned();
        let ids: Vec<String> = controller.queue_rows().into_iter().map(|r| r.id).collect();
        let client = account.client.clone();
        let result = runtime::run(async move { client.create_playlist(&name, &ids).await }).await;
        let Some(win) = app.window() else { return };
        match result {
            Ok(_) => {
                win.toast(&gettext("Queue saved as a playlist"));
                app.library().sync(false);
            }
            Err(e) => win.toast(&crate::widgets::error_toast::message(&e)),
        }
    }
}

fn row_factory(controller: &PlaybackController) -> gtk::SignalListItemFactory {
    let c1 = controller.downgrade();
    let c2 = controller.downgrade();
    let c3 = controller.downgrade();
    editable_rows::factory(editable_rows::EditOps {
        remove: Rc::new(move |pos| {
            if let Some(c) = c1.upgrade() {
                c.remove_at(pos);
            }
        }),
        move_item: Rc::new(move |from, to| {
            if let Some(c) = c2.upgrade() {
                c.move_item(from, to);
            }
        }),
        len: Rc::new(move || {
            c3.upgrade()
                .map_or(0, |c| c.queue_model().n_items() as usize)
        }),
    })
}
