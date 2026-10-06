#!/bin/bash
# Build "Midna Dev" from this checkout, install it to ~/Applications/Midna Dev.app and (re)start
# it. It runs side by side with your real Midna and never touches it: its own bundle id
# (com.mrgnhnt.midna.dev, so its own Accessibility/notification grants), its own daemon
# (label com.mrgnhnt.midna.dev.daemon), its own home and its own CLI link. The home is built
# in (--dev-home), so the MIDNA_HOME / MIDNA_SOCKET a Midna terminal exports can't point it at
# the real daemon, and it refuses to write the real Midna's files. It never updates itself, and
# has an inverted icon and the Gruvbox theme. Its terminals survive a rerun (daemon upgraded in
# place).
#
#   scripts/dev-app.sh [--test] [--no-build]
#     --test       run `cargo test --workspace` first and stop if it fails
#     --no-build   skip the build; just reinstall and restart from the last dev build
#   scripts/dev-app.sh --uninstall   quit it, stop and unregister its daemon, delete the app
#                                    (its home stays; delete it by hand to start fresh)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ID="com.mrgnhnt.midna.dev"
LABEL="$ID.daemon"
DEV_HOME="$HOME/Library/Application Support/$ID"
DEST="$HOME/Applications/Midna Dev.app"
OUT="$ROOT/dist/dev"
LOG="$DEV_HOME/app.log"

TEST=0 BUILD=1 UNINSTALL=0
for a in "$@"; do
  case "$a" in
    --test) TEST=1 ;;
    --no-build) BUILD=0 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "dev-app.sh: unknown option $a" >&2; exit 2 ;;
  esac
done

quit_dev_app() {
  local pid
  pid="$(pgrep -f "$DEST/Contents/MacOS/midna-app" | head -1 || true)"
  [ -n "$pid" ] || return 0
  echo "==> quit Midna Dev (pid $pid)"
  osascript -e "tell application id \"$ID\" to quit" >/dev/null
  for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.5; done
  echo "Midna Dev didn't quit; leaving it running" >&2
  return 1
}

if [ "$UNINSTALL" = 1 ]; then
  quit_dev_app
  if [ -x "$DEST/Contents/MacOS/midna-app" ]; then
    MIDNA_HOME="$DEV_HOME" "$DEST/Contents/MacOS/midna-app" --uninstall || true
  fi
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  rm -rf "$DEST"
  echo "==> removed $DEST (home kept: $DEV_HOME)"
  exit 0
fi

source ./env.sh >/dev/null
export LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast

if [ "$TEST" = 1 ]; then
  echo "==> cargo test"
  if ! cargo test --workspace; then
    echo "tests failed; not installing" >&2
    exit 1
  fi
fi

if [ "$BUILD" = 1 ]; then
  echo "==> build Midna Dev"
  # Own target dir, so dev and release builds don't rebuild each other (different update key).
  packaging/build-app.sh --out "$OUT" --target-dir "$ROOT/target/dev-app" \
    --bundle-id "$ID" --label "$LABEL" --name "Midna Dev" --icon packaging/assets/MidnaDev.icns --no-update-key \
    --dev-home "$DEV_HOME" --env "MIDNA_HOME=$DEV_HOME" 2>&1 | grep -E "==> built|^error" || true
fi
[ -d "$OUT/Midna.app" ] || { echo "no app at $OUT/Midna.app (run without --no-build)" >&2; exit 1; }

# Copy next to the destination first, so the running copy is only quit once the new one is ready.
mkdir -p "$(dirname "$DEST")"
NEW="$(dirname "$DEST")/.Midna Dev.new.app" OLD="$(dirname "$DEST")/.Midna Dev.old.app"
rm -rf "$NEW" "$OLD"
ditto "$OUT/Midna.app" "$NEW"
quit_dev_app || { rm -rf "$NEW"; exit 1; }
[ -e "$DEST" ] && mv "$DEST" "$OLD"
mv "$NEW" "$DEST"
rm -rf "$OLD"

echo "==> open $DEST"
open "$DEST"
# Wait for the app to report on its daemon (upgrade in place, or already current).
for _ in $(seq 1 30); do
  sleep 0.5
  line="$(tail -n 5 "$LOG" 2>/dev/null | grep -E "daemon\.upgrade|is current" | tail -1 || true)"
  if [ -n "$line" ] && [[ "$line" == *"pid $(pgrep -f "$DEST/Contents/MacOS/midna-app" | head -1) "* ]]; then
    echo "    ${line#*Z }"
    break
  fi
done
# A fresh dev home starts on the default theme; give it Gruvbox so it can't pass for the real
# Midna (Twilight). A theme picked by hand is left alone.
DEV_CLI=(env MIDNA_HOME="$DEV_HOME" MIDNA_SOCKET="$DEV_HOME/midnad.sock" "$DEV_HOME/bin/current/midna")
if [ "$("${DEV_CLI[@]}" settings get theme 2>/dev/null)" = "system" ]; then
  "${DEV_CLI[@]}" settings set theme gruvbox >/dev/null && echo "    theme: gruvbox"
fi
echo "==> done"
