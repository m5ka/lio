#!/usr/bin/env bash
# Regenerates Lio's icons and branding images from the artwork in data/branding/.
#
#   data/branding/lio.png       app icon (square, transparent, >= 512 px)
#   data/branding/lio_full.png  logo with the name, for About and the README
#
# Writes (all PNG, optimised):
#   data/icons/hicolor/<N>x<N>/apps/dev.m5ka.Lio.png        installed app icon
#   data/icons/hicolor/<N>x<N>/apps/dev.m5ka.Lio.Devel.png  development builds (striped)
#   data/resources/icons/hicolor/<N>x<N>/apps/lio-branding.png  About dialog (bundled)
#   data/screenshots/logo.png                               README
#
# Requires ImageMagick 7 (`magick`) and `optipng`. Re-run after changing the artwork.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/data/branding"
icons="$root/data/icons/hicolor"
bundled="$root/data/resources/icons/hicolor"
app_id="dev.m5ka.Lio"
sizes=(16 24 32 48 64 128 256 512)

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

# Development variant: translucent diagonal stripes over the icon's opaque parts.
magick -size 48x48 xc:none -fill '#1a1a1a70' \
    -draw 'polygon 0,0 12,0 48,36 48,48 36,48 0,12' \
    -draw 'polygon 36,0 48,0 48,12' -draw 'polygon 0,36 12,48 0,48' \
    "$work/stripe.png"
magick "$src/lio.png" \
    \( -size "$(magick identify -format '%wx%h' "$src/lio.png")" tile:"$work/stripe.png" \) \
    -compose ATop -composite "$work/devel.png"

# Downscale; small sizes get a little sharpening so the ring and play symbol stay crisp.
resize() { # resize <in> <size> <out>
    local sharpen=()
    (($2 <= 64)) && sharpen=(-unsharp 0x0.6+0.8+0.01)
    mkdir -p "$(dirname "$3")"
    magick "$1" -filter Lanczos -resize "$2x$2" "${sharpen[@]}" -strip "PNG32:$3"
    optipng -quiet -o2 "$3"
}

for size in "${sizes[@]}"; do
    dir="$icons/${size}x${size}/apps"
    resize "$src/lio.png" "$size" "$dir/$app_id.png"
    resize "$work/devel.png" "$size" "$dir/$app_id.Devel.png"
done

for size in 128 256; do
    resize "$src/lio_full.png" "$size" "$bundled/${size}x${size}/apps/lio-branding.png"
done
resize "$src/lio_full.png" 512 "$root/data/screenshots/logo.png"

echo "icons written to $icons and $bundled"
