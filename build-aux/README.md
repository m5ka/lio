# Build and maintenance files

- `check-potfiles.sh`: checks (or with `--update`, rewrites) `po/POTFILES.in`. Runs in CI.
- `make-icons.sh`: regenerates the app icons and logos from `data/branding/`.
- `screenshots.sh`: regenerates `data/screenshots/` from the fixture library on the test server.
- `dist-vendor.sh`: vendors crates into `meson dist` tarballs.
- `aur/`: the `lio-git` AUR package.

## Flatpak

- `dev.m5ka.Lio.Devel.json`: development build. It downloads crates during the build (`--share=network`).
- `dev.m5ka.Lio.json`: release build. It builds offline from `cargo-sources.json`.

`cargo-sources.json` lists every crate in `Cargo.lock` so the release build can fetch them
up front. Regenerate it after every `Cargo.lock` change:

```sh
python3 flatpak-cargo-generator.py Cargo.lock -o build-aux/cargo-sources.json
```

(`flatpak-cargo-generator.py` comes from https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)

The glycin image loaders are part of the GNOME 49 runtime, so no extra module is needed for them.
