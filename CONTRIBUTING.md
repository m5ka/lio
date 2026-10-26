# Contributing to Lio

## Layout

```text
src/                    GTK/libadwaita app (session, playback controller, library sync)
  widgets/              Widgets; content pages in widgets/pages/
  desktop/              MPRIS, tray icon, notifications, background playback
  dev/                  Debug-build helpers and frame-timing instrumentation
data/resources/ui/      Blueprint UI files (compiled by build.rs)
crates/lio-subsonic/    Subsonic/OpenSubsonic client: auth, endpoints, types (no GTK)
crates/lio-store/       SQLite mirror, sync, search, queue, TrackStore (stream/cache),
                        downloads, scrobbling (no GTK)
crates/lio-playback/    GStreamer: playbin3 player (gapless, ReplayGain) and the
                        `liosrc` element that reads through the TrackStore (no GTK)
crates/lio-bench/       Synthetic large-library benchmark
tests/integration/      Tests against a real Navidrome (tests/test-server.sh)
build-aux/              Flatpak manifests, AUR package, icon and maintenance scripts
```

The library crates have no GTK dependency so their tests run headless.

## Tests

```sh
cargo test --workspace                                    # unit + contract tests

tests/fixtures/generate-music.sh                          # synthetic CC0 test library
tests/test-server.sh up                                   # Navidrome in Docker on :4533
cargo test -p lio --test integration -- --ignored         # integration tests

cargo run --release -p lio-bench -- --tracks 100000       # performance budgets
```

The playback tests in `lio-playback` use the generated library and skip themselves when
it's missing (as in packaging builds). Set `LIO_REQUIRE_FIXTURES=1` to make them fail
instead; CI does.

Before sending a change: `cargo fmt --all` and
`cargo clippy --workspace --all-targets -- -D warnings`.

## Debug helpers (debug builds only)

| Variable | Effect |
|----------|--------|
| `LIO_TEST_SERVER`, `LIO_TEST_USER`, `LIO_TEST_PASSWORD` | Sign in without the keyring |
| `LIO_TEST_OFFLINE=1` | Open the saved library offline |
| `LIO_TEST_AUTOPLAY_ALBUM=<id>` | Play an album after sign-in |
| `LIO_TEST_SHUFFLE=1` | With `LIO_TEST_AUTOPLAY_ALBUM`, press the album page's Shuffle button instead |
| `LIO_TEST_NOW_PLAYING=1`, `LIO_TEST_LYRICS=1`, `LIO_TEST_FULLSCREEN=1` | With `LIO_TEST_AUTOPLAY_ALBUM`, open Now Playing (with lyrics expanded, fullscreen) |
| `LIO_LRCLIB_URL=<url>` | Use a mock LRCLIB server for online lyrics |
| `LIO_TEST_SECTION=<home\|albums\|…\|search>` or `playlist:<name>`, `LIO_TEST_SEARCH=<text>` | Open a section or sidebar playlist |
| `LIO_TEST_ACTION=<app.x\|win.x>` | Activate an action (opens dialogs) |
| `LIO_TEST_SIZE=<w>x<h>` | Initial window size |
| `LIO_SCREENSHOT=<file.png>`, `LIO_SCREENSHOT_DELAY_MS` | Render the window to a PNG and quit |
| `LIO_AUDIO_SINK=fakesink` | Play without sound |
| `LIO_FRAME_STATS=1`, `LIO_AUTOSCROLL=albums\|tracks` | Frame timing / scripted scroll |

Use `GSETTINGS_BACKEND=memory` and temporary `XDG_DATA_HOME`/`XDG_CACHE_HOME`
directories to keep test runs away from your real settings and data.

## Translations

Strings are marked with `gettext`/`ngettext` in Rust and `_()` in Blueprint. After
adding the first translatable string to a file (or moving one), run
`build-aux/check-potfiles.sh --update` to regenerate `po/POTFILES.in`; CI fails while
it is stale. Update the template with `meson compile -C build lio-pot`.

## Icons and branding

The artwork lives in `data/branding/`: `lio.png` (the app icon) and
`lio_full.png` (the logo with the name, used in About and the README). After changing
either, run `build-aux/make-icons.sh` (needs ImageMagick 7 and `optipng`). It writes the
app icon at 16–512 px to `data/icons/hicolor/<N>x<N>/apps/` (plus the striped
`.Devel` variant for development builds), the bundled About logo to
`data/resources/icons/hicolor/`, and the README logo `data/screenshots/logo.png`. The one-colour
`data/icons/hicolor/symbolic/apps/dev.m5ka.Lio-symbolic.svg` is drawn by hand.

## Screenshots

`build-aux/screenshots.sh` regenerates `data/screenshots/` (home, album, Now Playing and
the 360 px mobile layout) from the fixture library. It needs a graphical session and the
test server (`tests/fixtures/generate-music.sh`, then `tests/test-server.sh up`), and
uses a throwaway profile, so your own settings and library are untouched. Re-run it after
visible UI changes.

## Releasing

1. Bump the version in `Cargo.toml` (`workspace.package`) and `meson.build`, and add a
   `<release>` entry to `data/dev.m5ka.Lio.metainfo.xml.in`.
2. If `Cargo.lock` changed since the last release, regenerate
   `build-aux/cargo-sources.json` (see `build-aux/README.md`); the offline release
   Flatpak can't build with a stale one.
3. Build the release manifest once: `flatpak-builder --force-clean flatpak-build build-aux/dev.m5ka.Lio.json`.
