//! Playlist page: header and a virtualised track list.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{CompositeTemplate, gio, glib};
use lio_store::{queries, sync};

use crate::application::LioApplication;
use crate::covers::SIZE_GRID;
use crate::models::{IdListModel, ItemKind, Playlist, format_duration};
use crate::playback_controller::QueueOrigin;
use crate::runtime;
use crate::widgets::{add_to_playlist, cover_view, editable_rows, error_toast, track_list_row};

/// Reorders are sent to the server after this pause, so a series of moves is one request.
const PUSH_DELAY: std::time::Duration = std::time::Duration::from_millis(1200);

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/playlist_page.ui")]
    pub struct LioPlaylistPage {
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub cover_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub cover_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub cover_picture: TemplateChild<gtk::Image>,
        #[template_child]
        pub name_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub meta_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub action_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub track_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub edit_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub more_button: TemplateChild<gtk::MenuButton>,
        pub push_pending: Cell<bool>,
        pub playlist: RefCell<Option<Playlist>>,
        pub model: RefCell<Option<IdListModel>>,
        pub cancellable: RefCell<Option<gio::Cancellable>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioPlaylistPage {
        const NAME: &'static str = "LioPlaylistPage";
        type Type = super::LioPlaylistPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.install_action("playlist.play", None, |page, _, _| page.play_from(0, false));
            klass.install_action("playlist.shuffle", None, |page, _, _| {
                page.play_from(0, true)
            });
            klass.install_action_async("playlist.rename", None, |page, _, _| async move {
                page.rename().await
            });
            klass.install_action_async("playlist.delete", None, |page, _, _| async move {
                page.delete().await
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioPlaylistPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.cover_bin.set_size_request(128, 128);
            self.track_list
                .set_factory(Some(&track_list_row::factory()));
            self.track_list.connect_activate(glib::clone!(
                #[weak(rename_to = page)]
                self.obj(),
                move |_, position| {
                    if !page.imp().edit_button.is_active() {
                        page.play_from(position as usize, false);
                    }
                }
            ));
            self.edit_button.connect_toggled(glib::clone!(
                #[weak(rename_to = page)]
                self.obj(),
                move |b| page.set_editing(b.is_active())
            ));
        }
    }

    impl WidgetImpl for LioPlaylistPage {}
    impl NavigationPageImpl for LioPlaylistPage {}
}

glib::wrapper! {
    pub struct LioPlaylistPage(ObjectSubclass<imp::LioPlaylistPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioPlaylistPage {
    pub fn new(playlist: &Playlist) -> Self {
        let obj: Self = glib::Object::builder()
            .property("title", playlist.name())
            .build();
        let imp = obj.imp();
        imp.playlist.replace(Some(playlist.clone()));
        if let Some(account) = LioApplication::default().session().account() {
            let model = IdListModel::new(ItemKind::Track, &account.db);
            imp.track_list
                .set_model(Some(&gtk::NoSelection::new(Some(model.clone()))));
            imp.model.replace(Some(model));
        }
        obj.show(playlist);
        obj.reload();
        obj.refresh_from_server();
        let editable = !playlist.readonly();
        obj.imp().edit_button.set_visible(editable);
        obj.imp().more_button.set_visible(editable);
        let id = playlist.id().to_owned();
        let toggle = crate::widgets::download_toggle::new(
            lio_store::downloads::PinKind::Playlist,
            "playlist",
            &id,
        );
        obj.imp().action_box.append(&toggle);
        obj
    }

    fn set_editing(&self, editing: bool) {
        let imp = self.imp();
        if editing {
            let weak = self.downgrade();
            let (w1, w2, w3) = (weak.clone(), weak.clone(), weak);
            imp.track_list
                .set_factory(Some(&editable_rows::factory(editable_rows::EditOps {
                    remove: Rc::new(move |pos| {
                        if let Some(p) = w1.upgrade() {
                            p.remove_track(pos);
                        }
                    }),
                    move_item: Rc::new(move |from, to| {
                        if let Some(p) = w2.upgrade() {
                            p.move_track(from, to);
                        }
                    }),
                    len: Rc::new(move || {
                        w3.upgrade()
                            .and_then(|p| {
                                p.imp()
                                    .model
                                    .borrow()
                                    .as_ref()
                                    .map(|m| m.n_items() as usize)
                            })
                            .unwrap_or(0)
                    }),
                })));
        } else {
            imp.track_list.set_factory(Some(&track_list_row::factory()));
        }
    }

    fn window_toast(&self, message: &str) {
        if let Some(win) = LioApplication::default().window() {
            win.toast(message);
        }
    }

    fn remove_track(&self, pos: usize) {
        let Some(model) = self.imp().model.borrow().clone() else {
            return;
        };
        let mut ids = model.ids();
        if pos >= ids.len() {
            return;
        }
        ids.remove(pos);
        model.set_ids(ids);
        let (Some(playlist), Some(account)) = (
            self.playlist(),
            LioApplication::default().session().account(),
        ) else {
            return;
        };
        let id = playlist.id().to_owned();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let client = account.client.clone();
                let result =
                    runtime::run(
                        async move { client.update_playlist(&id, None, &[], &[pos]).await },
                    )
                    .await;
                page.after_edit(result).await;
            }
        ));
    }

    fn move_track(&self, from: usize, to: usize) {
        let imp = self.imp();
        let Some(model) = imp.model.borrow().clone() else {
            return;
        };
        let mut ids = model.ids();
        if from >= ids.len() || to >= ids.len() {
            return;
        }
        let id = ids.remove(from);
        ids.insert(to, id);
        model.set_ids(ids);
        if imp.push_pending.replace(true) {
            return;
        }
        glib::timeout_add_local_once(
            PUSH_DELAY,
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move || {
                    page.imp().push_pending.set(false);
                    glib::spawn_future_local(async move { page.push_order().await });
                }
            ),
        );
    }

    /// Sends the current order (replacing the playlist's content).
    async fn push_order(&self) {
        let (Some(model), Some(playlist), Some(account)) = (
            self.imp().model.borrow().clone(),
            self.playlist(),
            LioApplication::default().session().account(),
        ) else {
            return;
        };
        let rowids = model.ids();
        let rows = account
            .db
            .read(|c| queries::track_rows(c, &rowids))
            .unwrap_or_default();
        let by_rowid: std::collections::HashMap<i64, String> =
            rows.into_iter().map(|r| (r.rowid, r.id)).collect();
        let song_ids: Vec<String> = rowids
            .iter()
            .filter_map(|r| by_rowid.get(r).cloned())
            .collect();
        let (client, id) = (account.client.clone(), playlist.id().to_owned());
        let result =
            runtime::run(async move { client.replace_playlist_tracks(&id, &song_ids).await }).await;
        self.after_edit(result).await;
    }

    /// Re-syncs the playlist from the server after an edit (or reports the error).
    async fn after_edit(&self, result: Result<(), lio_subsonic::Error>) {
        if let Err(e) = result {
            self.window_toast(&error_toast::message(&e));
        }
        self.refresh_from_server();
        LioApplication::default().library().emit_changed();
    }

    async fn rename(&self) {
        let Some(playlist) = self.playlist() else {
            return;
        };
        let Some(name) =
            add_to_playlist::ask_name(self, &gettext("Rename Playlist"), playlist.name()).await
        else {
            return;
        };
        let Some(account) = LioApplication::default().session().account() else {
            return;
        };
        let (client, id, new_name) = (
            account.client.clone(),
            playlist.id().to_owned(),
            name.clone(),
        );
        let result =
            runtime::run(
                async move { client.update_playlist(&id, Some(&new_name), &[], &[]).await },
            )
            .await;
        if result.is_ok() {
            self.set_title(&name);
        }
        self.after_edit(result).await;
    }

    async fn delete(&self) {
        let Some(playlist) = self.playlist() else {
            return;
        };
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Delete Playlist?"))
            .body(gettext("“{}” will be deleted from the server.").replace("{}", playlist.name()))
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("delete", &gettext("_Delete")),
        ]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        if dialog.choose_future(Some(self)).await != "delete" {
            return;
        }
        let Some(account) = LioApplication::default().session().account() else {
            return;
        };
        let (client, id) = (account.client.clone(), playlist.id().to_owned());
        let result = runtime::run(async move { client.delete_playlist(&id).await }).await;
        match result {
            Ok(()) => {
                let rowid = playlist.rowid();
                let _ = account.db.with(|c| {
                    Ok(c.execute("DELETE FROM playlist WHERE rowid = ?1", [rowid])
                        .map(drop)?)
                });
                LioApplication::default().library().emit_changed();
                if let Some(win) = LioApplication::default().window() {
                    // Opened from the sidebar it is the only page: go Home instead.
                    if !win.nav_view().pop() {
                        win.show_section(crate::widgets::window::Section::Home);
                    }
                    win.toast(&gettext("Playlist deleted"));
                }
            }
            Err(e) => self.window_toast(&error_toast::message(&e)),
        }
    }

    pub fn playlist(&self) -> Option<Playlist> {
        self.imp().playlist.borrow().clone()
    }

    fn show(&self, playlist: &Playlist) {
        let imp = self.imp();
        let row = playlist.row();
        imp.name_label.set_label(playlist.name());
        let songs = ngettext("{} song", "{} songs", row.song_count.max(0) as u32)
            .replace("{}", &row.song_count.to_string());
        let mut meta = vec![songs, format_duration(row.duration_s)];
        if let Some(owner) = row.owner.as_deref().filter(|_| playlist.readonly()) {
            meta.insert(0, owner.to_owned());
        }
        imp.meta_label.set_label(&meta.join(" · "));
        cover_view::show_cover(
            &imp.cover_stack,
            &*imp.cover_picture,
            row.cover_art_id.as_deref(),
            SIZE_GRID,
            &imp.cancellable,
        );
    }

    pub fn reload(&self) {
        let imp = self.imp();
        let (Some(model), Some(playlist)) = (imp.model.borrow().clone(), self.playlist()) else {
            return;
        };
        let Some(account) = LioApplication::default().session().account() else {
            return;
        };
        let rowid = playlist.rowid();
        model.set_ids(
            account
                .db
                .read(|c| queries::playlist_track_ids(c, rowid))
                .unwrap_or_default(),
        );
        if let Some(row) = account
            .db
            .read(|c| queries::playlist_row(c, rowid))
            .ok()
            .flatten()
        {
            let fresh = Playlist::new(row);
            fresh.set_offline_available(playlist.offline_available());
            self.show(&fresh);
            imp.playlist.replace(Some(fresh));
        }
    }

    fn refresh_from_server(&self) {
        let session = LioApplication::default().session();
        let (Some(account), true) = (session.account(), session.is_online()) else {
            return;
        };
        let Some(id) = self.playlist().map(|p| p.id().to_owned()) else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let user = account.username.clone();
                let ok = runtime::run(async move {
                    sync::sync_playlist(&account.client, &account.db, &user, &id).await
                })
                .await
                .is_ok();
                if ok {
                    page.reload();
                }
            }
        ));
    }

    fn play_from(&self, index: usize, shuffle: bool) {
        let Some(playlist) = self.playlist() else {
            return;
        };
        let Some(account) = LioApplication::default().session().account() else {
            return;
        };
        let rowid = playlist.rowid();
        let tracks = account
            .db
            .read(|c| queries::playlist_track_rows(c, rowid))
            .unwrap_or_default();
        let origin = QueueOrigin::Playlist(playlist.id().to_owned());
        let playback = LioApplication::default().playback();
        if shuffle {
            playback.shuffle_tracks(tracks, origin);
        } else {
            playback.play_tracks(tracks, index, origin);
        }
    }
}
