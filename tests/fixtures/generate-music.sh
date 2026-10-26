#!/usr/bin/env bash
# Generate Lio's CC0 test library (synthetic audio only) into tests/fixtures/music/.
#
# Albums:
#   Lio Fixtures - Gapless Sweep       5 FLAC tracks cut sample-exactly from one continuous sweep
#                                      (synced lyrics on part 1, plain lyrics on part 2)
#   Lio Fixtures - ReplayGain          FLAC tracks with REPLAYGAIN_* tags (quiet and loud)
#   (untitled folder) Bare Album       no artist, no year, no cover
#   Ünïcödé Ärtist - 東京 / Long titles very long and non-Latin titles
#   Lio Fixtures - Format Mix          FLAC + Opus + MP3
#   Lio Fixtures - Long Tracks         4-minute MP3 and FLAC (several MB each), for seeking while
#                                      a track is still streaming
#
# Requires ffmpeg (with libopus and libmp3lame) and metaflac. Safe to re-run.
set -euo pipefail

OUT="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/music}"
RATE=44100
FF=(ffmpeg -hide_banner -loglevel error -y)

# Empty the folder rather than replacing it: a running test server has it mounted.
mkdir -p "$OUT"
find "$OUT" -mindepth 1 -delete

cover() { # cover <file> <colour>
    "${FF[@]}" -f lavfi -i "color=c=$2:s=600x600" -frames:v 1 "$1"
}

tag_flac() { # tag_flac <file> key=value...
    local f="$1"; shift
    metaflac --remove-all-tags "$f"
    local kv
    for kv in "$@"; do metaflac --set-tag="$kv" "$f"; done
}

# --- Gapless sweep: one continuous 100 s chirp, split into 5 x 20 s tracks at exact samples
dir="$OUT/Lio Fixtures/Gapless Sweep"
mkdir -p "$dir"
sweep="$(mktemp --suffix=.wav)"
trap 'rm -f "$sweep"' EXIT
"${FF[@]}" -f lavfi -i "aevalsrc='0.4*sin(2*PI*(200*t+ (1800/200)*t*t)):c=stereo:s=$RATE:d=100'" "$sweep"
for i in 1 2 3 4 5; do
    start=$(( (i - 1) * 20 * RATE ))
    end=$(( i * 20 * RATE ))
    f="$dir/0$i - Sweep Part $i.flac"
    "${FF[@]}" -i "$sweep" -af "atrim=start_sample=$start:end_sample=$end,asetpts=PTS-STARTPTS" -c:a flac "$f"
    tag_flac "$f" "ARTIST=Lio Fixtures" "ALBUMARTIST=Lio Fixtures" "ALBUM=Gapless Sweep" \
        "TITLE=Sweep Part $i" "TRACKNUMBER=$i" "TRACKTOTAL=5" "DATE=2026" "GENRE=Test Tones"
done
cover "$dir/cover.png" "0x3584e4"
# Lyrics: synced (.lrc sidecar) on part 1, unsynced (embedded tag) on part 2.
cat >"$dir/01 - Sweep Part 1.lrc" <<'LRC'
[ar:Lio Fixtures]
[ti:Sweep Part 1]
[00:00.50]The sweep begins low and slow
[00:04.00]Climbing through the frequencies
[00:08.00]Every cycle a little higher
[00:12.00]No gap between the parts
[00:16.00]Hand over to part two
LRC
metaflac --set-tag="LYRICS=Plain lyrics without timestamps
Second line of the verse

A new stanza after a blank line" "$dir/02 - Sweep Part 2.flac"

# --- ReplayGain album: quiet and loud tones with known gains
dir="$OUT/Lio Fixtures/ReplayGain"
mkdir -p "$dir"
n=1
for spec in "quiet:0.05:+12.50:0.050000" "loud:0.95:-8.20:0.950000" "medium:0.3:-1.00:0.300000"; do
    IFS=: read -r name amp gain peak <<<"$spec"
    f="$dir/0$n - Tone $name.flac"
    "${FF[@]}" -f lavfi -i "sine=frequency=440:sample_rate=$RATE:duration=15,volume=$amp" -ac 2 -c:a flac "$f"
    tag_flac "$f" "ARTIST=Lio Fixtures" "ALBUMARTIST=Lio Fixtures" "ALBUM=ReplayGain" \
        "TITLE=Tone $name" "TRACKNUMBER=$n" "DATE=2025" "GENRE=Test Tones" \
        "REPLAYGAIN_TRACK_GAIN=$gain dB" "REPLAYGAIN_TRACK_PEAK=$peak" \
        "REPLAYGAIN_ALBUM_GAIN=-2.00 dB" "REPLAYGAIN_ALBUM_PEAK=0.950000"
    n=$((n + 1))
done
cover "$dir/cover.png" "0xe66100"

# --- Bare album: only album + title tags, no artist/year/cover
dir="$OUT/untitled"
mkdir -p "$dir"
for i in 1 2; do
    f="$dir/track$i.flac"
    "${FF[@]}" -f lavfi -i "sine=frequency=$((300 * i)):sample_rate=$RATE:duration=8" -c:a flac "$f"
    tag_flac "$f" "ALBUM=Bare Album" "TITLE=Bare Track $i"
done

# --- Unicode and very long titles
dir="$OUT/Ünïcödé Ärtist/東京"
mkdir -p "$dir"
titles=(
    "Björk-ish Étude № 1"
    "東京の夜 (Tokyo Night)"
    "A Very Long Title That Keeps Going And Going Well Past Any Reasonable Width To Test Ellipsising In Every Single View Of The Application (Extended Remix Version Featuring Absolutely Everyone)"
)
for i in "${!titles[@]}"; do
    n=$((i + 1))
    f="$dir/0$n.flac"
    "${FF[@]}" -f lavfi -i "sine=frequency=$((500 + 100 * n)):sample_rate=$RATE:duration=10" -c:a flac "$f"
    tag_flac "$f" "ARTIST=Ünïcödé Ärtist" "ALBUMARTIST=Ünïcödé Ärtist" "ALBUM=東京" \
        "TITLE=${titles[$i]}" "TRACKNUMBER=$n" "DATE=2024" "GENRE=Électronique"
done
cover "$dir/cover.png" "0x9141ac"

# --- Format mix: FLAC, Opus, MP3
dir="$OUT/Lio Fixtures/Format Mix"
mkdir -p "$dir"
src="sine=frequency=660:sample_rate=48000:duration=12"
"${FF[@]}" -f lavfi -i "$src" -ac 2 -c:a flac "$dir/01 - FLAC.flac"
tag_flac "$dir/01 - FLAC.flac" "ARTIST=Lio Fixtures" "ALBUMARTIST=Lio Fixtures" "ALBUM=Format Mix" "TITLE=FLAC" "TRACKNUMBER=1" "DATE=2023" "GENRE=Test Tones"
"${FF[@]}" -f lavfi -i "$src" -ac 2 -c:a libopus -b:a 128k \
    -metadata artist="Lio Fixtures" -metadata album_artist="Lio Fixtures" -metadata album="Format Mix" \
    -metadata title="Opus" -metadata track=2 -metadata date=2023 -metadata genre="Test Tones" "$dir/02 - Opus.opus"
"${FF[@]}" -f lavfi -i "$src" -ac 2 -c:a libmp3lame -b:a 320k -id3v2_version 3 \
    -metadata artist="Lio Fixtures" -metadata album_artist="Lio Fixtures" -metadata album="Format Mix" \
    -metadata title="MP3" -metadata track=3 -metadata date=2023 -metadata genre="Test Tones" "$dir/03 - MP3.mp3"
cover "$dir/cover.png" "0x2ec27e"

# --- Long tracks: big enough that a seek lands megabytes beyond the download position
dir="$OUT/Lio Fixtures/Long Tracks"
mkdir -p "$dir"
"${FF[@]}" -f lavfi -i "sine=frequency=220:sample_rate=$RATE:duration=240" -ac 2 \
    -c:a libmp3lame -b:a 192k -id3v2_version 3 \
    -metadata artist="Lio Fixtures" -metadata album_artist="Lio Fixtures" -metadata album="Long Tracks" \
    -metadata title="Long MP3" -metadata track=1 -metadata date=2023 -metadata genre="Test Tones" \
    "$dir/01 - Long MP3.mp3"
# Noise, so the FLAC doesn't compress to almost nothing.
"${FF[@]}" -f lavfi -i "anoisesrc=duration=240:color=pink:amplitude=0.2:sample_rate=$RATE" -ac 2 \
    -c:a flac "$dir/02 - Long FLAC.flac"
tag_flac "$dir/02 - Long FLAC.flac" "ARTIST=Lio Fixtures" "ALBUMARTIST=Lio Fixtures" "ALBUM=Long Tracks" "TITLE=Long FLAC" "TRACKNUMBER=2" "DATE=2023" "GENRE=Test Tones"
cover "$dir/cover.png" "0xc64600"

echo "Fixture library written to $OUT"
find "$OUT" -type f | wc -l | xargs echo "files:"
