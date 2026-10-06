#!/bin/bash
# Renders packaging/assets/dmg/background.html to background.tiff, the DMG
# window's background, at 1x and 2x so it's sharp on Retina screens.
#
# Run it after changing background.html and commit the TIFF; releases use
# the committed file. Needs Chrome or Chromium (set CHROME to use a
# particular one) and site/node_modules for the font (cd site && npm install).
set -euo pipefail
cd "$(dirname "$0")/.."

dir=packaging/assets/dmg
font=site/node_modules/@fontsource-variable/inter
[ -d "$font" ] || { echo "Missing $font. Run: cd site && npm install" >&2; exit 1; }

chrome="${CHROME:-}"
if [ -z "$chrome" ]; then
  for candidate in \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium" \
    "$HOME"/Library/Caches/ms-playwright/chromium_headless_shell-*/*/chrome-headless-shell; do
    [ -x "$candidate" ] && chrome="$candidate" && break
  done
fi
[ -n "$chrome" ] || { echo "Chrome not found. Install it or set CHROME=/path/to/chrome." >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for scale in 1 2; do
  "$chrome" --headless --disable-gpu --hide-scrollbars --allow-file-access-from-files \
    --window-size=660,420 --force-device-scale-factor="$scale" \
    --screenshot="$tmp/background@${scale}x.png" "file://$PWD/$dir/background.html" >/dev/null 2>&1
done
sips -g pixelWidth -g pixelHeight "$tmp/background@1x.png" "$tmp/background@2x.png" | grep pixel
# One TIFF holding both sizes; Finder picks the one for the screen.
tiffutil -cathidpicheck "$tmp/background@1x.png" "$tmp/background@2x.png" -out "$dir/background.tiff"
echo "Wrote $dir/background.tiff"
