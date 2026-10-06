#!/bin/bash
# Rebuild dist/<version>/Midna.app and restart the running app on it. Terminals survive: on
# launch the app upgrades midnad in place (same PID, PTYs handed over).
#
#   scripts/reinstall.sh [--test] [--no-build]
#     --test       run `cargo test --workspace` first and stop if it fails
#     --no-build   skip the build; just restart the app from the existing bundle
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TEST=0 BUILD=1
for a in "$@"; do
  case "$a" in
    --test) TEST=1 ;;
    --no-build) BUILD=0 ;;
    -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
    *) echo "reinstall.sh: unknown option $a" >&2; exit 2 ;;
  esac
done

# This restarts the PRODUCTION app (the human's daily terminal). Human only: it asks on the
# terminal, which an agent's non-interactive shell can't answer. Agents use scripts/dev-app.sh.
if ! { [ -t 0 ] && [ -r /dev/tty ]; }; then
  echo "reinstall.sh restarts the production Midna and is human only (no terminal to confirm on)." >&2
  echo "Agents: use scripts/dev-app.sh (Midna Dev). See AGENTS.md." >&2
  exit 1
fi
read -r -p "Quit the PRODUCTION Midna and restart it from a dist/ build? Type 'production': " answer </dev/tty
[ "$answer" = production ] || { echo "cancelled"; exit 1; }

source ./env.sh >/dev/null
export LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
APP="$ROOT/dist/$VERSION/Midna.app"
LOG="$HOME/Library/Application Support/com.mrgnhnt.midna/app.log"

if [ "$TEST" = 1 ]; then
  echo "==> cargo test"
  if ! cargo test --workspace; then
    echo "tests failed; not reinstalling" >&2
    exit 1
  fi
fi

if [ "$BUILD" = 1 ]; then
  echo "==> build"
  packaging/build-app.sh | grep "==> built"
fi
[ -d "$APP" ] || { echo "no app at $APP (run without --no-build)" >&2; exit 1; }

PID="$(pgrep -f "$APP/Contents/MacOS/midna-app" || true)"
if [ -n "$PID" ]; then
  echo "==> quit running app (pid $PID)"
  osascript -e 'tell application id "com.mrgnhnt.midna" to quit' >/dev/null
  for _ in $(seq 1 20); do kill -0 "$PID" 2>/dev/null || break; sleep 0.5; done
  if kill -0 "$PID" 2>/dev/null; then echo "app didn't quit; leaving it running" >&2; exit 1; fi
fi

echo "==> open $APP"
open "$APP"
# Wait for the app to report on the daemon (upgrade in place, or already current).
for _ in $(seq 1 30); do
  sleep 0.5
  line="$(tail -n 5 "$LOG" 2>/dev/null | grep -E "daemon\.upgrade|is current" | tail -1 || true)"
  if [ -n "$line" ] && [[ "$line" == *"pid $(pgrep -f "$APP/Contents/MacOS/midna-app" | head -1) "* ]]; then
    echo "    ${line#*Z }"
    break
  fi
done
echo "==> done"
