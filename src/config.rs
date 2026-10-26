//! Build-time configuration. Meson passes these via `LIO_*` environment variables;
//! plain `cargo build` falls back to development defaults.

pub const APP_ID: &str = match option_env!("LIO_APP_ID") {
    Some(id) => id,
    None if cfg!(debug_assertions) => "dev.m5ka.Lio.Devel",
    None => "dev.m5ka.Lio",
};

/// `"Devel"` for development builds, empty otherwise.
pub const PROFILE: &str = match option_env!("LIO_PROFILE") {
    Some(p) => p,
    None if cfg!(debug_assertions) => "Devel",
    None => "",
};

pub const VERSION: &str = match option_env!("LIO_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub const GETTEXT_PACKAGE: &str = match option_env!("LIO_GETTEXT_PACKAGE") {
    Some(p) => p,
    None => "lio",
};

pub const LOCALEDIR: &str = match option_env!("LIO_LOCALEDIR") {
    Some(d) => d,
    None => "/usr/share/locale",
};

/// Base path of the embedded GResource bundle (the same for every profile).
pub const RESOURCE_BASE: &str = "/dev/m5ka/Lio";

/// Compiled schema for non-installed runs; used when the schema isn't installed.
pub const DEV_SCHEMA_DIR: &str = env!("LIO_DEV_SCHEMA_DIR");

/// Settings schema ID, shared by every profile.
pub const SCHEMA_ID: &str = "dev.m5ka.Lio";

pub fn is_devel() -> bool {
    PROFILE == "Devel"
}

/// MPRIS bus name suffix (`org.mpris.MediaPlayer2.<this>`).
pub fn mpris_name() -> &'static str {
    if is_devel() { "lio.Devel" } else { "lio" }
}
