#!/usr/bin/env bash
# Manage a throwaway Navidrome instance for Lio's integration tests and manual checks.
#
#   tests/test-server.sh up [--tls-self-signed]   start (creating if needed), wait until ready
#   tests/test-server.sh down                      remove container and its data
#   tests/test-server.sh pause | resume            freeze / unfreeze (simulates a hung server)
#   tests/test-server.sh stop                      stop container (simulates server offline)
#   tests/test-server.sh status
#   tests/test-server.sh seed                      create test data (also part of `up`; CI runs
#                                                  it against its own Navidrome)
#
# Server: http://localhost:4533 (https with --tls-self-signed), admin user lio / lio-test.
# Seeded data: "Fixture Playlist" with Gapless Sweep parts 1 and 2.
set -euo pipefail

IMAGE="deluan/navidrome:0.64.2"
NAME="lio-test-navidrome"
PORT="${LIO_TEST_PORT:-4533}"
USER_NAME="lio"
USER_PASS="lio-test"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MUSIC="${LIO_TEST_MUSIC:-$ROOT/tests/fixtures/music}"
STATE="$ROOT/target/test-server"

scheme="http"
[[ -f "$STATE/tls" ]] && scheme="https"

die() { echo "test-server: $*" >&2; exit 1; }

url() { echo "$scheme://localhost:$PORT"; }

curl_srv() { curl -fsS --insecure --max-time 5 "$@"; }

wait_ready() {
    local i
    for i in $(seq 1 60); do
        if curl_srv "$(url)/ping" >/dev/null 2>&1 || curl_srv "$(url)/app/" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    die "server did not come up on $(url)"
}

ensure_admin() {
    # First-run endpoint: only succeeds while no user exists, so failures are ignored.
    curl_srv -X POST -H 'Content-Type: application/json' \
        -d "{\"username\":\"$USER_NAME\",\"password\":\"$USER_PASS\"}" \
        "$(url)/auth/createAdmin" >/dev/null 2>&1 || true
}

rest() {
    local endpoint="$1"; shift
    curl_srv "$(url)/rest/$endpoint?u=$USER_NAME&p=$USER_PASS&v=1.16.1&c=lio-test&f=json$*"
}

wait_scan() {
    local i out
    for i in $(seq 1 120); do
        out="$(rest getScanStatus || true)"
        if [[ "$out" == *'"scanning":false'* && "$out" != *'"count":0'* ]]; then
            return 0
        fi
        sleep 1
    done
    echo "test-server: warning: scan did not finish in time ($out)" >&2
}

# Creates the playlist the integration tests expect, unless it exists already.
seed() {
    if rest getPlaylists | grep -q '"name":"Fixture Playlist"'; then
        return 0
    fi
    local album songs
    album="$(rest getAlbumList2 "&type=alphabeticalByName&size=500" | python3 -c '
import json, sys
albums = json.load(sys.stdin)["subsonic-response"]["albumList2"].get("album", [])
print(next((a["id"] for a in albums if a["name"] == "Gapless Sweep"), ""))')"
    [[ -n "$album" ]] || die "can't seed: Gapless Sweep isn't in the library"
    songs="$(rest getAlbum "&id=$album" | python3 -c '
import json, sys
songs = json.load(sys.stdin)["subsonic-response"]["album"]["song"]
print("".join("&songId=" + s["id"] for s in songs[:2]))')"
    rest createPlaylist "&name=Fixture%20Playlist$songs" >/dev/null
    echo "seeded Fixture Playlist"
}

# Removes the server's files. They may belong to root (containers from before `--user`),
# so fall back to deleting them from inside a container.
remove_state() {
    rm -rf "$STATE" 2>/dev/null && return 0
    docker run --rm --user 0 --entrypoint rm -v "$STATE:/state" "$IMAGE" -rf /state/data /state/tls-files
    rm -rf "$STATE"
}

cmd_up() {
    local tls=0
    [[ "${1:-}" == "--tls-self-signed" ]] && tls=1
    [[ -d "$MUSIC" ]] || die "fixture library missing at $MUSIC (run tests/fixtures/generate-music.sh)"
    mkdir -p "$STATE/data"

    if docker container inspect "$NAME" >/dev/null 2>&1; then
        docker start "$NAME" >/dev/null
    else
        local args=(
            -d --name "$NAME"
            # Files in $STATE stay yours, so `down` can delete them.
            --user "$(id -u):$(id -g)"
            -p "$PORT:4533"
            -v "$MUSIC:/music:ro"
            -v "$STATE/data:/data"
            -e ND_LOGLEVEL=info
            -e ND_SCANONSTARTUP=true
            -e ND_ENABLETRANSCODINGCONFIG=true
            -e ND_DEFAULTDOWNSAMPLINGFORMAT=opus
        )
        rm -f "$STATE/tls"
        if (( tls )); then
            mkdir -p "$STATE/tls-files"
            openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj "/CN=localhost" \
                -keyout "$STATE/tls-files/key.pem" -out "$STATE/tls-files/cert.pem" 2>/dev/null
            args+=(-v "$STATE/tls-files:/tls:ro" -e ND_TLSCERT=/tls/cert.pem -e ND_TLSKEY=/tls/key.pem)
            touch "$STATE/tls"
            scheme="https"
        fi
        docker run "${args[@]}" "$IMAGE" >/dev/null
    fi

    wait_ready
    ensure_admin
    wait_scan
    seed
    echo "Navidrome ready at $(url) (user $USER_NAME / $USER_PASS)"
}

case "${1:-}" in
    up) shift; cmd_up "$@" ;;
    down) docker rm -f "$NAME" >/dev/null 2>&1 || true; remove_state; echo "removed" ;;
    seed) seed ;;
    pause) docker pause "$NAME" >/dev/null && echo "paused" ;;
    resume) docker unpause "$NAME" >/dev/null && echo "resumed" ;;
    stop) docker stop "$NAME" >/dev/null && echo "stopped" ;;
    status) docker ps -a --filter "name=^${NAME}$" --format '{{.Status}}' ;;
    *) sed -n '2,13p' "$0"; exit 1 ;;
esac
