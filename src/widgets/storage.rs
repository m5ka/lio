//! Storage preferences page: cache limit and usage, Clear Cache, download quality,
//! downloading on metered networks.

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};

use crate::application::LioApplication;
use crate::runtime;

const QUALITIES: [&str; 4] = ["original", "opus-192", "opus-128", "mp3-320"];

fn usage_text() -> String {
    let Some(account) = LioApplication::default().session().account() else {
        return String::new();
    };
    match account.tracks.usage() {
        Ok(u) => gettext("Cache {cache} · Downloads {downloads}")
            .replace("{cache}", &glib::format_size(u.cache_bytes))
            .replace("{downloads}", &glib::format_size(u.download_bytes)),
        Err(_) => String::new(),
    }
}

/// Adds the storage groups to `page`.
pub fn populate(page: &adw::PreferencesPage) {
    let app = LioApplication::default();
    let settings: &gio::Settings = app.settings();

    let cache = adw::PreferencesGroup::builder()
        .title(gettext("Audio Cache"))
        .description(gettext(
            "Recently played music is kept so it starts instantly and plays offline",
        ))
        .build();
    let limit = adw::SpinRow::builder()
        .title(gettext("Cache Size Limit"))
        .subtitle(gettext("In MB; least-played songs are removed first"))
        .adjustment(&gtk::Adjustment::new(
            2048.0,
            100.0,
            1_048_576.0,
            256.0,
            1024.0,
            0.0,
        ))
        .build();
    settings.bind("cache-limit-mb", &limit, "value").build();
    let usage = adw::ActionRow::builder()
        .title(gettext("Used"))
        .subtitle(usage_text())
        .css_classes(["property"])
        .build();
    let clear = adw::ButtonRow::builder()
        .title(gettext("_Clear Cache"))
        .use_underline(true)
        .build();
    clear.connect_activated(glib::clone!(
        #[weak]
        usage,
        move |_| {
            let Some(account) = LioApplication::default().session().account() else {
                return;
            };
            let store = account.tracks.clone();
            glib::spawn_future_local(async move {
                let _ = runtime::run(async move {
                    tokio::task::spawn_blocking(move || store.clear_cache()).await
                })
                .await;
                usage.set_subtitle(&usage_text());
                if let Some(win) = LioApplication::default().window() {
                    win.toast(&gettext("Cache cleared"));
                }
            });
        }
    ));
    cache.add(&limit);
    cache.add(&usage);
    cache.add(&clear);

    let downloads = adw::PreferencesGroup::builder()
        .title(gettext("Downloads"))
        .build();
    let quality = adw::ComboRow::builder()
        .title(gettext("Download Quality"))
        .model(&gtk::StringList::new(&[
            &gettext("Original"),
            &gettext("High (Opus 192 kbps)"),
            &gettext("Standard (Opus 128 kbps)"),
            &gettext("MP3 320 kbps"),
        ]))
        .build();
    let current = settings.string("download-quality");
    quality.set_selected(
        QUALITIES
            .iter()
            .position(|q| *q == current.as_str())
            .unwrap_or(0) as u32,
    );
    let s = settings.clone();
    quality.connect_selected_notify(move |row| {
        if let Some(q) = QUALITIES.get(row.selected() as usize) {
            let _ = s.set_string("download-quality", q);
        }
    });
    let metered = adw::SwitchRow::builder()
        .title(gettext("Download on Metered Networks"))
        .subtitle(gettext("Mobile data and hotspots may cost money"))
        .build();
    settings
        .bind("download-on-metered", &metered, "active")
        .build();
    let manage = adw::ButtonRow::builder()
        .title(gettext("_Manage Downloads"))
        .use_underline(true)
        .end_icon_name("go-next-symbolic")
        .action_name("win.go-downloads")
        .build();
    downloads.add(&quality);
    downloads.add(&metered);
    downloads.add(&manage);

    page.add(&cache);
    page.add(&downloads);
}
