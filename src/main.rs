//! Lio: a native GNOME music player for Navidrome.

mod actions;
mod application;
mod config;
mod covers;
mod desktop;
mod dev;
mod library_sync;
mod lyrics;
mod models;
mod offline;
mod playback_controller;
mod runtime;
mod scrobbler;
mod session;
mod settings;
mod widgets;

use gtk::prelude::*;
use gtk::{gio, glib};

use application::LioApplication;

fn main() -> glib::ExitCode {
    runtime::process_start();
    init_tracing();

    // GTK calls setlocale(LC_ALL, "") itself during initialisation.
    let _ = gettextrs::bindtextdomain(config::GETTEXT_PACKAGE, config::LOCALEDIR);
    let _ = gettextrs::bind_textdomain_codeset(config::GETTEXT_PACKAGE, "UTF-8");
    let _ = gettextrs::textdomain(config::GETTEXT_PACKAGE);

    gio::resources_register_include!("lio.gresource").expect("failed to register resources");

    // Let the main thread spawn Tokio tasks (zbus runs in Tokio mode for MPRIS and
    // portals); the GTK main loop still drives the UI.
    let _tokio = runtime::runtime().enter();
    glib::set_application_name("Lio");

    LioApplication::new().run()
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| {
        EnvFilter::new("lio=info,lio_store=info,lio_subsonic=info,lio_playback=info")
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
