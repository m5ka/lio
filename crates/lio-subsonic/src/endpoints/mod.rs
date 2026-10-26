//! Endpoint wrappers, grouped by API area.

mod annotation;
mod browse;
mod media;
mod playlists;
mod system;

pub use annotation::StarTarget;
pub use browse::{ALBUM_PAGE_SIZE, AlbumListType, SearchLimits};
pub use media::{COVER_SIZES, StreamResponse};
