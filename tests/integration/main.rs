//! Integration tests against a real Navidrome (`tests/test-server.sh up`).
//!
//! Every test is `#[ignore]`; run with `cargo test -p lio --test integration -- --ignored`.

mod common;
mod mpris;
mod offline;
mod scrobble_once;
mod seek_while_streaming;
mod sign_in_play;
mod sync;
mod tls;
