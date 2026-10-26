#!/usr/bin/env bash
# Regenerates the screenshots in data/screenshots/ from the fixture library.
#
# Needs a debug build, a graphical session, and the test server with the fixture
# library (tests/fixtures/generate-music.sh, then tests/test-server.sh up).
# Settings and data go to a temporary directory, so your own setup is untouched.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/data/screenshots"
server="${LIO_TEST_SERVER:-http://localhost:4533}"
user="lio"
pass="lio-test"

rest() {
    curl -fsS "$server/rest/$1?u=$user&p=$pass&v=1.16.1&c=lio-screenshots&f=json$2"
}

album_id() {
    rest getAlbumList2 "&type=alphabeticalByName&size=100" |
        python3 -c 'import json, sys
for a in json.load(sys.stdin)["subsonic-response"]["albumList2"]["album"]:
    if a["name"] == sys.argv[1]:
        print(a["id"])' "$1"
}

album="$(album_id "Gapless Sweep")"
[[ -n "$album" ]] || { echo "screenshots: Gapless Sweep not found on $server" >&2; exit 1; }

# The album and its first track are starred in the screenshots.
song="$(rest getAlbum "&id=$album" | python3 -c 'import json, sys
print(json.load(sys.stdin)["subsonic-response"]["album"]["song"][0]["id"])')"
rest star "&albumId=$album&id=$song" >/dev/null

cargo build --manifest-path "$root/Cargo.toml" -p lio
bin="$root/target/debug/lio"

# shot <file> <size> [VAR=value...]: one sign-in in a fresh profile, then a screenshot.
shot() {
    local file="$1" size="$2"
    shift 2
    local tmp
    tmp="$(mktemp -d)"
    env GSETTINGS_BACKEND=memory \
        XDG_DATA_HOME="$tmp/data" XDG_CACHE_HOME="$tmp/cache" XDG_STATE_HOME="$tmp/state" \
        LIO_TEST_SERVER="$server" LIO_TEST_USER="$user" LIO_TEST_PASSWORD="$pass" \
        LIO_AUDIO_SINK=fakesink LIO_TEST_SIZE="$size" \
        LIO_SCREENSHOT="$out/$file" LIO_SCREENSHOT_DELAY_MS="${DELAY_MS:-6000}" \
        "$@" "$bin"
    rm -rf "$tmp"
    echo "wrote data/screenshots/$file"
}

shot home.png 1100x760 LIO_TEST_SECTION=home
shot album.png 1100x760 LIO_TEST_AUTOPLAY_ALBUM="$album"
shot now-playing.png 1100x760 LIO_TEST_AUTOPLAY_ALBUM="$album" LIO_TEST_NOW_PLAYING=1
shot mobile.png 360x720 LIO_TEST_AUTOPLAY_ALBUM="$album"

if command -v optipng >/dev/null; then
    optipng -quiet -o2 "$out"/{home,album,now-playing,mobile}.png
fi
