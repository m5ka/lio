//! Shared helpers for tests that play the generated fixture library.

use std::path::PathBuf;

/// The fixture album folder `name`, or `None` (test skipped) when the library hasn't been
/// generated. With `LIO_REQUIRE_FIXTURES` set (CI), a missing library fails instead.
pub fn fixture_album(name: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/music/Lio Fixtures")
        .join(name);
    if dir.is_dir() {
        return Some(dir);
    }
    let hint = format!("missing {dir:?}; run tests/fixtures/generate-music.sh");
    assert!(std::env::var_os("LIO_REQUIRE_FIXTURES").is_none(), "{hint}");
    eprintln!("skipped: {hint}");
    None
}
