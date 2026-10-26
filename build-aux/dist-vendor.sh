#!/bin/sh
# Vendor crates into the dist tarball so it builds offline (Flathub, distros).
set -e
export DIST="$1"
export SOURCE_ROOT="$2"

cd "$SOURCE_ROOT"
mkdir -p "$DIST/.cargo"
cargo vendor --locked "$DIST/vendor" | sed "s|$DIST/||" > "$DIST/.cargo/config.toml"
