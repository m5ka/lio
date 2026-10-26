//! Typed client for the Subsonic / OpenSubsonic API as served by Navidrome, plus the
//! LRCLIB lyrics lookup ([`lrclib`]).
//!
//! No GTK dependency, so it is fully testable headless.

mod auth;
mod client;
mod endpoints;
mod error;
pub mod lrclib;
pub mod types;

pub use auth::{API_VERSION, AuthMode, CLIENT_NAME, Credentials, token};
pub use client::{
    Client, Params, TlsConfig, fingerprint, is_insecure_remote, normalize_base_url, redact,
};
pub use endpoints::{
    ALBUM_PAGE_SIZE, AlbumListType, COVER_SIZES, SearchLimits, StarTarget, StreamResponse,
};
pub use error::{Error, Result};
