//! Typed access to the `dev.m5ka.Lio` GSettings schema.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use lio_store::Quality;

use crate::config;

/// Thin typed wrapper; derefs to [`gio::Settings`] for binding and signals.
#[derive(Clone, Debug)]
pub struct Settings(gio::Settings);

impl std::ops::Deref for Settings {
    type Target = gio::Settings;
    fn deref(&self) -> &gio::Settings {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayGainMode {
    Off,
    Track,
    Album,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumsSort {
    RecentlyAdded,
    Name,
    Artist,
    Year,
    MostPlayed,
    Random,
}

impl AlbumsSort {
    pub const ALL: [Self; 6] = [
        Self::RecentlyAdded,
        Self::Name,
        Self::Artist,
        Self::Year,
        Self::MostPlayed,
        Self::Random,
    ];

    pub fn nick(self) -> &'static str {
        match self {
            Self::RecentlyAdded => "recently-added",
            Self::Name => "name",
            Self::Artist => "artist",
            Self::Year => "year",
            Self::MostPlayed => "most-played",
            Self::Random => "random",
        }
    }

    pub fn from_nick(nick: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|s| s.nick() == nick)
            .unwrap_or(Self::RecentlyAdded)
    }
}

impl Settings {
    /// Uses the installed schema, or the copy compiled by `build.rs` for
    /// non-installed development runs.
    pub fn new() -> Self {
        let source = gio::SettingsSchemaSource::default();
        let installed = source
            .as_ref()
            .and_then(|s| s.lookup(config::SCHEMA_ID, true));
        let schema = installed.or_else(|| {
            gio::SettingsSchemaSource::from_directory(
                config::DEV_SCHEMA_DIR,
                source.as_ref(),
                false,
            )
            .ok()
            .and_then(|s| s.lookup(config::SCHEMA_ID, false))
        });
        let schema = schema.expect("GSettings schema dev.m5ka.Lio not found");
        Self(gio::Settings::new_full(
            &schema,
            None::<&gio::SettingsBackend>,
            None,
        ))
    }

    pub fn server_url(&self) -> String {
        self.string("server-url").into()
    }

    pub fn username(&self) -> String {
        self.string("username").into()
    }

    pub fn auth_mode(&self) -> lio_subsonic::AuthMode {
        lio_subsonic::AuthMode::from_str_lossy(&self.string("auth-mode"))
    }

    pub fn pinned_cert(&self) -> Option<String> {
        let fp: String = self.string("pinned-cert-sha256").into();
        (!fp.is_empty()).then_some(fp)
    }

    pub fn stream_quality(&self, metered: bool) -> Quality {
        let key = if metered {
            "stream-quality-metered"
        } else {
            "stream-quality-unmetered"
        };
        Quality::parse(&self.string(key)).unwrap_or_default()
    }

    pub fn download_quality(&self) -> Quality {
        Quality::parse(&self.string("download-quality")).unwrap_or_default()
    }

    pub fn download_on_metered(&self) -> bool {
        self.boolean("download-on-metered")
    }

    pub fn cache_limit_bytes(&self) -> u64 {
        u64::from(self.uint("cache-limit-mb")) * 1024 * 1024
    }

    pub fn replaygain(&self) -> ReplayGainMode {
        match self.string("replaygain").as_str() {
            "off" => ReplayGainMode::Off,
            "track" => ReplayGainMode::Track,
            "album" => ReplayGainMode::Album,
            _ => ReplayGainMode::Auto,
        }
    }

    pub fn scrobble(&self) -> bool {
        self.boolean("scrobble")
    }

    pub fn notifications(&self) -> bool {
        self.boolean("notifications")
    }

    pub fn close_to_background(&self) -> bool {
        self.boolean("close-to-background")
    }

    pub fn offline_mode(&self) -> bool {
        self.boolean("offline-mode")
    }

    pub fn volume(&self) -> f64 {
        self.double("volume")
    }

    pub fn albums_sort(&self) -> AlbumsSort {
        AlbumsSort::from_nick(&self.string("albums-sort"))
    }

    /// Calls `f` whenever `key` changes.
    pub fn connect_changed_key<F: Fn(&Self) + 'static>(
        &self,
        key: &str,
        f: F,
    ) -> glib::SignalHandlerId {
        let this = self.clone();
        self.0.connect_changed(Some(key), move |_, _| f(&this))
    }

    /// Clears every account-related key (sign-out).
    pub fn reset_account(&self) {
        for key in [
            "server-url",
            "username",
            "auth-mode",
            "pinned-cert-sha256",
            "server-type",
            "server-version",
            "api-version",
            "extensions",
        ] {
            self.reset(key);
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::new()
    }
}
