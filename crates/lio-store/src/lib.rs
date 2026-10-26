//! Local persistence for Lio: library mirror, queue, audio cache, downloads, scrobbles.
//!
//! No GTK dependency; everything here is testable headless.

pub mod cache;
pub mod db;
pub mod downloads;
pub mod error;
pub mod library;
pub mod model;
pub mod paths;
pub mod queries;
pub mod queue;
pub mod ranges;
pub mod scrobble;
pub mod search;
pub mod sync;
pub mod track_store;

pub use db::Db;
pub use error::{Error, Result};
pub use model::{AlbumRow, ArtistRow, PlaylistRow, Quality, TrackRow};
pub use paths::{AccountPaths, wipe_account};
pub use track_store::{Origin, READ_TIMEOUT, TrackHandle, TrackStore};
