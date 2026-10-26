//! GObject models wrapping `lio-store` rows.

mod album;
mod artist;
mod id_list_model;
mod playlist;
mod track;

pub use album::Album;
pub use artist::Artist;
pub use id_list_model::{IdListModel, ItemKind};
pub use playlist::Playlist;
pub use track::Track;

use gettextrs::gettext;

/// The artist string, or "Unknown Artist" when the metadata is missing.
pub fn display_artist(artist: Option<&str>) -> String {
    match artist {
        Some(a) if !a.trim().is_empty() => a.to_owned(),
        _ => gettext("Unknown Artist"),
    }
}

/// `m:ss` or `h:mm:ss`.
pub fn format_duration(seconds: i64) -> String {
    let s = seconds.max(0);
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
