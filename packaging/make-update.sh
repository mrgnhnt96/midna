#!/bin/bash
# Package a built Midna.app as an update: the archive plus a signed feed entry.
#   packaging/make-update.sh --app dist/0.1.1/Midna.app --url-base https://midna.dev/updates/files \
#       [--channel stable] [--notes "…"] [--out dist/updates] [--key ~/.config/midna-dev/update-ed25519.key]
#       [--arch universal|aarch64|x86_64] [--min-macos 12.0]
# Writes <out>/Midna-<version>.app.tar.gz and <out>/<channel>.json. Upload the archive to
# <url-base>/ and the JSON to the feed URL (updates.feed_url, {channel} -> channel).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="" URL_BASE="" CHANNEL=stable NOTES="" OUT="$ROOT/dist/updates" KEY="${MIDNA_UPDATE_KEY:-$HOME/.config/midna-dev/update-ed25519.key}"
ARCH="$(uname -m | sed 's/arm64/aarch64/')" MIN=12.0
while [ $# -gt 0 ]; do
  case "$1" in
    --app) APP="$2"; shift 2 ;;
    --url-base) URL_BASE="${2%/}"; shift 2 ;;
    --channel) CHANNEL="$2"; shift 2 ;;
    --notes) NOTES="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --key) KEY="$2"; shift 2 ;;
    --arch) ARCH="$2"; shift 2 ;;
    --min-macos) MIN="$2"; shift 2 ;;
    -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
    *) echo "make-update.sh: unknown option $1" >&2; exit 2 ;;
  esac
done
[ -d "$APP" ] && [ -n "$URL_BASE" ] || { echo "usage: make-update.sh --app PATH/Midna.app --url-base URL" >&2; exit 2; }
[ -f "$KEY" ] || { echo "no signing key at $KEY (packaging/gen-update-key.sh makes a dev one)" >&2; exit 1; }
/usr/bin/codesign --verify --deep --strict "$APP"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
TOOL="$ROOT/packaging/release-tool/target/release/midna-release"
cargo build --release -q --manifest-path "$ROOT/packaging/release-tool/Cargo.toml"
mkdir -p "$OUT"
ARCHIVE="$OUT/Midna-$VERSION.app.tar.gz"
# COPYFILE_DISABLE: no AppleDouble ._ files (they'd break the code signature check).
( cd "$(dirname "$APP")" && COPYFILE_DISABLE=1 tar -czf "$ARCHIVE" "$(basename "$APP")" )
"$TOOL" feed "$KEY" "$ARCHIVE" --version "$VERSION" --url "$URL_BASE/$(basename "$ARCHIVE")" \
  --notes "$NOTES" --min-macos "$MIN" --arch "$ARCH" > "$OUT/$CHANNEL.json"
"$TOOL" verify "$OUT/$CHANNEL.json" "$("$TOOL" pubkey "$KEY")" "$ARCHIVE" >/dev/null
echo "wrote $ARCHIVE"
echo "wrote $OUT/$CHANNEL.json:"
cat "$OUT/$CHANNEL.json"
