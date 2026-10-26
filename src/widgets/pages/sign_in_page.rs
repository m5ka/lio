//! Sign-in page: server address, username, password; inline errors.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::glib::subclass::Signal;
use gtk::{CompositeTemplate, glib};
use lio_subsonic::Error;

use crate::application::LioApplication;
use crate::widgets::{cert_dialog, error_toast};

mod imp {
    use std::sync::LazyLock;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/sign_in_page.ui")]
    pub struct LioSignInPage {
        #[template_child]
        pub server_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub username_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub password_row: TemplateChild<adw::PasswordEntryRow>,
        #[template_child]
        pub http_warning: TemplateChild<gtk::Label>,
        #[template_child]
        pub error_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub sign_in_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub sign_in_stack: TemplateChild<gtk::Stack>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioSignInPage {
        const NAME: &'static str = "LioSignInPage";
        type Type = super::LioSignInPage;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_instance_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioSignInPage {
        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                // Emitted after a successful sign-in; the flag means "limited compatibility".
                vec![
                    Signal::builder("signed-in")
                        .param_types([bool::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for LioSignInPage {
        fn grab_focus(&self) -> bool {
            if self.server_row.text().is_empty() {
                self.server_row.grab_focus()
            } else {
                self.password_row.grab_focus()
            }
        }
    }

    impl BinImpl for LioSignInPage {}
}

glib::wrapper! {
    pub struct LioSignInPage(ObjectSubclass<imp::LioSignInPage>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

#[gtk::template_callbacks]
impl LioSignInPage {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Pre-fills server and username (re-authentication keeps the queue).
    pub fn prefill(&self, server: &str, username: &str) {
        let imp = self.imp();
        imp.server_row.set_text(server);
        imp.username_row.set_text(username);
        imp.password_row.set_text("");
    }

    pub fn show_error(&self, message: &str) {
        let label = &self.imp().error_label;
        label.set_label(message);
        label.set_visible(true);
        self.announce(message, gtk::AccessibleAnnouncementPriority::High);
    }

    pub fn connect_signed_in<F: Fn(&Self, bool) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "signed-in",
            false,
            glib::closure_local!(move |obj: &Self, limited: bool| f(obj, limited)),
        )
    }

    #[template_callback]
    fn on_server_changed(&self) {
        let text = self.imp().server_row.text();
        let insecure = lio_subsonic::normalize_base_url(&text)
            .map(|u| lio_subsonic::is_insecure_remote(&u))
            .unwrap_or(false);
        self.imp().http_warning.set_visible(insecure);
    }

    #[template_callback]
    fn on_entry_activated(&self) {
        self.imp().sign_in_button.emit_clicked();
    }

    #[template_callback]
    fn on_sign_in_clicked(&self) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move { page.sign_in().await }
        ));
    }

    fn set_busy(&self, busy: bool) {
        let imp = self.imp();
        imp.sign_in_stack
            .set_visible_child_name(if busy { "spinner" } else { "label" });
        for widget in [
            imp.sign_in_button.upcast_ref::<gtk::Widget>(),
            imp.server_row.upcast_ref(),
            imp.username_row.upcast_ref(),
            imp.password_row.upcast_ref(),
        ] {
            widget.set_sensitive(!busy);
        }
    }

    async fn sign_in(&self) {
        let imp = self.imp();
        let server = imp.server_row.text().to_string();
        let username = imp.username_row.text().trim().to_owned();
        let password = imp.password_row.text().to_string();
        imp.error_label.set_visible(false);
        if server.trim().is_empty() || username.is_empty() || password.is_empty() {
            self.show_error(&gettext("Enter a server address, username and password"));
            return;
        }

        let session = LioApplication::default().session();
        self.set_busy(true);
        let mut result = session.sign_in(&server, &username, &password, None).await;
        if let Err(Error::Certificate { fingerprint_sha256 }) = &result {
            let fingerprint = fingerprint_sha256.clone();
            if cert_dialog::ask_trust(self, &fingerprint).await {
                result = session
                    .sign_in(&server, &username, &password, Some(fingerprint))
                    .await;
            } else {
                result = Err(Error::Certificate {
                    fingerprint_sha256: fingerprint,
                });
            }
        }
        self.set_busy(false);

        match result {
            Ok(info) => {
                imp.password_row.set_text("");
                self.emit_by_name::<()>("signed-in", &[&info.limited_compatibility]);
            }
            Err(e) => {
                tracing::info!("sign-in failed: {e}");
                self.show_error(&error_toast::message(&e));
                imp.password_row.grab_focus();
            }
        }
    }
}

impl Default for LioSignInPage {
    fn default() -> Self {
        Self::new()
    }
}
