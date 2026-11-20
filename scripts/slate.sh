#!/bin/bash
# Screenshot or drive this checkout's midna-app hidden, with Slate: the window opens below the
# desktop, never takes focus, and midna never becomes the frontmost app, so nothing shows on
# your screen and the app you're using keeps the keyboard.
#
#   scripts/slate.sh shoot --out shot.png [--size 1280x800] [--wait 2500]
#   scripts/slate.sh drive --steps steps.json --out shots/ [--size 1280x800] [--wait 2500]
#   scripts/slate.sh --home /tmp/mh.XXXX shoot …   # against a temp daemon's home, not the fake
#
# Everything after the subcommand goes to `slate` (see its README for the steps file); the app
# is added after `--`. The app runs on the fake backend (MIDNA_BACKEND=fake) unless --home
# names a temp MIDNA_HOME with a running midnad; the production home is refused, since the app
# connects as the human. Other MIDNA_* screenshot variables (MIDNA_DEBUG_SCREEN,
# MIDNA_FAKE_SETTINGS, MIDNA_W, …) pass through to the app.
#
# It builds midna-app with the `snapshot` feature, so the app renders its own shots (no Screen
# Recording permission, no native traffic lights), and installs the `slate` command at the rev
# crates/midna-app/Cargo.toml pins into <target>/slate.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

usage() {
  sed -n '6,8p' "$0" | sed 's/^# *//' >&2
  exit 2
}

HOME_DIR=""
if [ "${1:-}" = "--home" ]; then
  [ $# -ge 2 ] || usage
  HOME_DIR="$2"
  shift 2
fi
case "${1:-}" in shoot | drive) ;; *) usage ;; esac

if [ -n "$HOME_DIR" ]; then
  HOME_DIR="$(cd "$HOME_DIR" && pwd)"
  case "$HOME_DIR" in
    "$HOME/Library/Application Support/com.mrgnhnt.midna"*)
      echo "slate.sh: $HOME_DIR is a real Midna home; use a temp one (see README, Dev mode)" >&2
      exit 1
      ;;
  esac
  [ -S "$HOME_DIR/midnad.sock" ] || { echo "slate.sh: no midnad running in $HOME_DIR" >&2; exit 1; }
fi

# shellcheck disable=SC1091
. ./env.sh
# This checkout's own target dir: a Midna terminal may point cargo at another checkout's.
export CARGO_TARGET_DIR="$ROOT/target"
echo "slate.sh: building midna-app" >&2
cargo build --quiet -p midna-app --features snapshot
app="$CARGO_TARGET_DIR/debug/midna-app"

rev=$(sed -n 's/^slate = {.*rev = "\([0-9a-f]*\)".*/\1/p' crates/midna-app/Cargo.toml)
[ -n "$rev" ] || { echo "slate.sh: can't find slate's rev in crates/midna-app/Cargo.toml" >&2; exit 1; }
# cargo skips the install when that rev is already there.
cargo install --quiet --locked --git https://github.com/mrgnhnt96/slate --rev "$rev" --root "$CARGO_TARGET_DIR/slate" slate-cli
slate="$CARGO_TARGET_DIR/slate/bin/slate"

# The app runs in a folder of its own (below), so paths given to slate are made absolute first.
args=()
want_path=0
for a in "$@"; do
  if [ "$want_path" = 1 ]; then
    case "$a" in /*) ;; *) a="$PWD/$a" ;; esac
    want_path=0
  else
    case "$a" in
      --out | --steps) want_path=1 ;;
      --out=/* | --steps=/*) ;;
      --out=* | --steps=*) a="${a%%=*}=$PWD/${a#*=}" ;;
    esac
  fi
  args+=("$a")
done

# A Midna terminal exports the socket of the daemon it runs in; the app must never reach it.
unset MIDNA_SOCKET MIDNA_SESSION
run=$(mktemp -d "${TMPDIR:-/tmp}/midna-slate.XXXXXX")
trap 'rm -rf "$run"' EXIT
# The app opens on the first-run setup screen unless app-state.json says it's finished
# (MIDNA_DEBUG_ONBOARDING=1 keeps it). The fake backend reads that file from the current
# folder; a temp home has its own.
state='{"onboarding":{"finished":true}}'
if [ -n "$HOME_DIR" ]; then
  export MIDNA_HOME="$HOME_DIR" MIDNA_SOCKET="$HOME_DIR/midnad.sock"
  unset MIDNA_BACKEND
  [ -n "${MIDNA_DEBUG_ONBOARDING:-}" ] || [ -e "$HOME_DIR/app-state.json" ] || echo "$state" > "$HOME_DIR/app-state.json"
else
  unset MIDNA_HOME
  export MIDNA_BACKEND=fake
  [ -n "${MIDNA_DEBUG_ONBOARDING:-}" ] || echo "$state" > "$run/app-state.json"
fi
# A hidden run never spawns a daemon of its own.
export MIDNA_NO_SPAWN=1
cd "$run"
"$slate" "${args[@]}" -- "$app"
