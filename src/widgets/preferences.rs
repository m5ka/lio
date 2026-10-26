//! Preferences: playback, storage, library and account pages.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, gio, glib};

use crate::application::LioApplication;

/// Settings nicks in the order of the combo rows' models.
const QUALITIES: [&str; 4] = ["original", "opus-192", "opus-128", "mp3-320"];
const REPLAYGAIN: [&str; 4] = ["off", "track", "album", "auto"];
const NOW_PLAYING_VIEWS: [&str; 5] = ["cover", "waves", "cd", "cd-spectrum", "visualizer"];

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/preferences.ui")]
    pub struct LioPreferences {
        #[template_child]
        pub stream_unmetered_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub stream_metered_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub replaygain_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub now_playing_view_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub fade_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub fade_duration_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub close_to_background_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub storage_page: TemplateChild<adw::PreferencesPage>,
        #[template_child]
        pub scrobble_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub notifications_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub online_lyrics_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub server_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub user_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub version_row: TemplateChild<adw::ActionRow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioPreferences {
        const NAME: &'static str = "LioPreferences";
        type Type = super::LioPreferences;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioPreferences {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().bind_settings();
        }
    }

    impl WidgetImpl for LioPreferences {}
    impl AdwDialogImpl for LioPreferences {}
    impl PreferencesDialogImpl for LioPreferences {}
}

glib::wrapper! {
    pub struct LioPreferences(ObjectSubclass<imp::LioPreferences>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Binds a combo row to an enum key whose nicks are listed in `nicks` (row order).
fn bind_combo(
    settings: &gio::Settings,
    key: &'static str,
    row: &adw::ComboRow,
    nicks: &'static [&'static str],
) {
    let current = settings.string(key);
    row.set_selected(
        nicks
            .iter()
            .position(|n| *n == current.as_str())
            .unwrap_or(0) as u32,
    );
    let settings = settings.clone();
    row.connect_selected_notify(move |row| {
        if let Some(nick) = nicks.get(row.selected() as usize) {
            let _ = settings.set_string(key, nick);
        }
    });
}

impl LioPreferences {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn storage_page(&self) -> adw::PreferencesPage {
        self.imp().storage_page.get()
    }

    fn bind_settings(&self) {
        let imp = self.imp();
        let app = LioApplication::default();
        let settings: &gio::Settings = app.settings();
        bind_combo(
            settings,
            "stream-quality-unmetered",
            &imp.stream_unmetered_row,
            &QUALITIES,
        );
        bind_combo(
            settings,
            "stream-quality-metered",
            &imp.stream_metered_row,
            &QUALITIES,
        );
        bind_combo(settings, "replaygain", &imp.replaygain_row, &REPLAYGAIN);
        bind_combo(
            settings,
            "now-playing-view",
            &imp.now_playing_view_row,
            &NOW_PLAYING_VIEWS,
        );
        settings
            .bind("fade-on-pause", &imp.fade_row.get(), "active")
            .build();
        settings
            .bind("fade-duration", &imp.fade_duration_row.get(), "value")
            .build();
        settings
            .bind("fade-on-pause", &imp.fade_duration_row.get(), "sensitive")
            .get()
            .build();
        settings
            .bind(
                "close-to-background",
                &imp.close_to_background_row.get(),
                "active",
            )
            .build();
        settings
            .bind("scrobble", &imp.scrobble_row.get(), "active")
            .build();
        settings
            .bind("notifications", &imp.notifications_row.get(), "active")
            .build();
        settings
            .bind("online-lyrics", &imp.online_lyrics_row.get(), "active")
            .build();

        imp.server_row.set_subtitle(&settings.string("server-url"));
        imp.user_row.set_subtitle(&settings.string("username"));
        let version = format!(
            "{} {}",
            settings.string("server-type"),
            settings.string("server-version")
        );
        imp.version_row.set_subtitle(version.trim());
        crate::widgets::storage::populate(&self.storage_page());
    }
}

impl Default for LioPreferences {
    fn default() -> Self {
        Self::new()
    }
}
