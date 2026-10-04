#!/bin/bash
# End-to-end install + auto-update test (docs/RELEASING.md, "End-to-end test").
#
# Builds TEST-flavored v0.1.0 and v0.1.1 bundles (bundle id com.mrgnhnt.midna.test, label
# com.mrgnhnt.midna.test.daemon, MIDNA_HOME in a temp dir via LSEnvironment + the plist's
# EnvironmentVariables), installs v0.1.0 to ~/Applications/MidnaTest/Midna.app, lets it
# register its LaunchAgent with SMAppService, opens a shell session, publishes v0.1.1 on a
# 127.0.0.1 feed, and waits for the app to download, verify, swap, relaunch and upgrade the
# daemon in place. Then checks the shell survived (same id, pid and screen) and cleans up
# everything: app quit, `--uninstall` (daemon.stop + SMAppService unregister), installs, the
# test MIDNA_HOME, the HTTP server and LaunchServices registrations.
#
#   packaging/e2e/update-e2e.sh [--no-build] [--keep]
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
BUILD=1 KEEP=0
for a in "$@"; do case "$a" in --no-build) BUILD=0 ;; --keep) KEEP=1 ;; esac; done

WORK="$ROOT/packaging/e2e/work"
TH="/tmp/mdt-e2e"                 # test MIDNA_HOME (short: Unix socket path limit)
BID="com.mrgnhnt.midna.test"
LABEL="$BID.daemon"
PORT=8791
DEST_DIR="$HOME/Applications/MidnaTest"
APP="$DEST_DIR/Midna.app"
FEED="http://127.0.0.1:$PORT/{channel}.json"
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
SRV_PID=""
PASS=0 FAIL=0

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
ok() { printf '  \033[32mPASS\033[0m %s\n' "$*"; PASS=$((PASS+1)); }
bad() { printf '  \033[31mFAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); }
check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }
cli() { MIDNA_HOME="$TH" MIDNA_SOCKET="$TH/midnad.sock" "$TH/bin/current/midna" "$@"; }
app_pids() { pgrep -f "$APP/Contents/MacOS/midna-app" || true; }
wait_for() {  # <secs> <cmd...>
  local n=$(( $1 * 5 )); shift
  while [ $n -gt 0 ]; do if eval "$*"; then return 0; fi; sleep 0.2; n=$((n-1)); done; return 1
}

cleanup() {
  say "cleanup"
  for p in $(app_pids); do kill "$p" 2>/dev/null; done
  sleep 1
  for p in $(app_pids); do kill -9 "$p" 2>/dev/null; done
  if [ -x "$APP/Contents/MacOS/midna-app" ]; then
    "$APP/Contents/MacOS/midna-app" --uninstall 2>&1 | sed 's/^/  /'
  fi
  # belt and braces: nothing of ours may stay loaded
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null
  pkill -f "$TH/bin/" 2>/dev/null
  [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null
  if [ "$KEEP" = 0 ]; then
    [ -d "$APP" ] && "$LSREGISTER" -u "$APP" 2>/dev/null
    rm -rf "$DEST_DIR" "$TH" "$WORK/srv"
    rmdir "$HOME/Applications" 2>/dev/null   # only if we created it and it's empty
  fi
  if launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1; then echo "  WARNING: $LABEL still loaded"; else echo "  $LABEL not loaded"; fi
}
trap cleanup EXIT

ENVS=(--env "MIDNA_HOME=$TH" --env "MIDNA_UPDATE_FEED_URL=$FEED" --env "MIDNA_UPDATE_CHECK_SECS=6"
      --env "MIDNA_DEBUG_UPDATE=apply" --env "MIDNA_CLI_LINK_DIR=$TH/localbin" --env "MIDNA_NO_ACTIVATE=1"
      --env "MIDNA_SECRETS=file")
FLAVOR=(--bundle-id "$BID" --label "$LABEL" --dev-drivers "${ENVS[@]}")

if [ "$BUILD" = 1 ]; then
  say "build v0.1.1 and v0.1.0 (test flavor)"
  packaging/build-app.sh --version 0.1.1 --out "$WORK/0.1.1" "${FLAVOR[@]}" || exit 1
  packaging/build-app.sh --version 0.1.0 --out "$WORK/0.1.0" "${FLAVOR[@]}" || exit 1
fi

say "start from a clean slate"
rm -rf "$TH" "$WORK/srv"; mkdir -p "$TH" "$WORK/srv"
if launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1; then echo "  $LABEL is already loaded; refusing"; exit 1; fi
echo '{"note":"no update yet"}' > "$WORK/srv/stable.json"
python3 -m http.server --bind 127.0.0.1 --directory "$WORK/srv" "$PORT" >"$WORK/http.log" 2>&1 &
SRV_PID=$!

say "install v0.1.0 to $APP"
mkdir -p "$DEST_DIR"; rm -rf "$APP"
/usr/bin/ditto "$WORK/0.1.0/Midna.app" "$APP"
open -n "$APP"
check "app 0.1.0 started" "wait_for 20 '[ -n \"\$(app_pids)\" ]'"
check "first launch installed midnad into bin/current" "wait_for 20 '[ -x $TH/bin/current/midnad ]'"
STATUS="$("$APP/Contents/MacOS/midna-app" --login-item-status 2>&1)"
echo "  SMAppService: $STATUS"
if [ "$STATUS" = "RequiresApproval" ]; then
  echo "  The login item needs approval in System Settings ▸ Login Items: that needs the human. Stopping here."
  exit 1
fi
check "login item enabled" "[ \"$STATUS\" = Enabled ]"
check "daemon up under launchd" "wait_for 20 '[ -S $TH/midnad.sock ] && cli info >/dev/null 2>&1'"
check "launchd job loaded" "launchctl print gui/$(id -u)/$LABEL >/dev/null 2>&1"
INFO0="$(cli info --json)"; echo "  $INFO0"
D_PID="$(echo "$INFO0" | python3 -c 'import json,sys; print(json.load(sys.stdin)["pid"])')"
check "daemon runs the installed copy (trampoline)" "echo '$INFO0' | grep -q 'mdt-e2e/bin/0.1.0-'"
check "daemon reports 0.1.0" "echo '$INFO0' | grep -q '\"version\": *\"0.1.0\"'"
check "CLI linked for agents" "[ \"\$(readlink $TH/localbin/midna)\" = $TH/bin/current/midna ]"

say "open a shell session"
SID="$(cli open --name e2e-shell --json -- /bin/zsh -f | python3 -c 'import json,sys; v=json.load(sys.stdin); print(v.get("id") or v["session"]["id"])')"
echo "  session $SID"
sleep 1
cli send "$SID" "echo survived-\$((6*7)) && echo shell-pid=\$\$" >/dev/null
wait_for 10 "cli read '$SID' | grep -q 'survived-42'"
SCREEN0="$(cli read "$SID" --screen)"
SHELL_PID="$(echo "$SCREEN0" | sed -n 's/^shell-pid=\([0-9]*\).*/\1/p' | tail -1)"
echo "  shell pid $SHELL_PID"
check "shell answered before the update" "echo \"\$SCREEN0\" | grep -q 'survived-42'"

say "publish v0.1.1 on the feed"
packaging/make-update.sh --app "$WORK/0.1.1/Midna.app" --url-base "http://127.0.0.1:$PORT" --out "$WORK/srv" --notes "e2e test update" >/dev/null || exit 1
echo "  $(python3 -c "import json; e=json.load(open('$WORK/srv/stable.json')); print(e['version'], e['sha256'][:16], e['size'])")"

say "wait for download, verify, swap, relaunch, daemon upgrade"
check "installed bundle is 0.1.1" "wait_for 60 '[ \"\$(/usr/libexec/PlistBuddy -c \"Print :CFBundleShortVersionString\" $APP/Contents/Info.plist 2>/dev/null)\" = 0.1.1 ]'"
check "app relaunched as 0.1.1" "wait_for 30 'grep -q \"v0.1.1 pid .* launch from\" $TH/app.log'"
check "daemon upgraded in place to 0.1.1" "wait_for 30 'cli info --json 2>/dev/null | grep -q \"\\\"version\\\": *\\\"0.1.1\\\"\"'"
INFO1="$(cli info --json)"; echo "  $INFO1"
D_PID1="$(echo "$INFO1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["pid"])')"
check "same daemon pid ($D_PID -> $D_PID1)" "[ '$D_PID' = '$D_PID1' ]"
check "daemon now runs bin/0.1.1-*" "echo '$INFO1' | grep -q 'mdt-e2e/bin/0.1.1-'"
check "session $SID still listed" "cli list --json | grep -q '$SID'"
check "shell pid $SHELL_PID alive" "kill -0 $SHELL_PID 2>/dev/null"
SCREEN1="$(cli read "$SID" --screen)"
check "screen kept (survived-42 still there)" "echo \"\$SCREEN1\" | grep -q 'survived-42'"
cli send "$SID" "echo after-update-\$((7*6))" >/dev/null
check "shell still interactive" "wait_for 10 \"cli read '$SID' | grep -q 'after-update-42'\""
check "exactly one app process" "[ \$(app_pids | wc -l) -eq 1 ]"
check "no leftover staging next to the app" "[ -z \"\$(ls -A $DEST_DIR | grep -v '^Midna.app\$')\" ]"
echo "  app.log:"; sed 's/^/    /' "$TH/app.log"
echo "  daemon events:"; cli events --kind daemon. 2>/dev/null | tail -5 | sed 's/^/    /'

say "result: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
