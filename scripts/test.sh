#!/bin/bash
# Run the tests in waves: build first (at low priority, so it doesn't starve tests already
# running), then wait for a machine-wide lock and run. One checkout's tests run at a time, so
# the integration tests' temp daemons and their 5-10s timeouts aren't fighting another
# agent's run for the CPU.
#
#   scripts/test.sh                          the whole workspace
#   scripts/test.sh -p midnad -p midna-cli   just those crates (see "Test what you touched")
#   scripts/test.sh -p midnad queue -- --nocapture
#
# Arguments are cargo test's. The lock is $MIDNA_TEST_LOCK (default /tmp/midna-tests.lock).
# Waiting runs line up in arrival order ($LOCK.line holds a ticket per waiting run), and the run
# holding the lock writes who it is to $LOCK.holder. While it waits, test.sh says on stderr where
# it stands and who holds the lock, when the wait starts and every minute after, so the agent (or
# person) running it can tell a long queue from a hang and decide to background it or wait.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck disable=SC1091
. "$ROOT/env.sh"
cd "$ROOT"
echo "test.sh: target=$CARGO_TARGET_DIR" >&2
LOCK="${MIDNA_TEST_LOCK:-/tmp/midna-tests.lock}"

scope=(--workspace) cargo_args=()
for a in "$@"; do
  [ "$a" = "--" ] && break
  case "$a" in -p|--package|--package=*|-p?*|--workspace) scope=() ;; esac
  cargo_args+=("$a")
done

nice -n 10 cargo test ${scope[@]+"${scope[@]}"} ${cargo_args[@]+"${cargo_args[@]}"} --no-run

LINE="$LOCK.line" HOLDER="$LOCK.holder"
REPORT_EVERY=60
mkdir -p "$LINE"
# Name order is arrival order: zero-padded seconds, then the pid. The pid is this script's, which
# lives until the tests end, so a ticket whose pid is gone belongs to a run that was killed.
ticket="$LINE/$(printf '%012d-%08d' "$(date +%s)" $$)"
printf '%s\n%s\n' "$ROOT" "$*" >"$ticket"
trap 'rm -f "$ticket"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# `45s`, `4m 10s`, `1h 5m`.
secs_label() {
  local s=$1
  if [ "$s" -lt 60 ]; then echo "${s}s"
  elif [ "$s" -lt 3600 ] && [ $((s % 60)) -eq 0 ]; then echo "$((s / 60))m"
  elif [ "$s" -lt 3600 ]; then echo "$((s / 60))m $((s % 60))s"
  else echo "$((s / 3600))h $((s % 3600 / 60))m"; fi
}

# The live tickets ahead of ours, oldest first; dead runs' tickets are dropped.
ahead() {
  local t pid
  for t in "$LINE"/*; do
    [ -e "$t" ] || continue
    [ "$t" \< "$ticket" ] || continue
    pid=$((10#${t##*-}))
    if kill -0 "$pid" 2>/dev/null; then echo "$t"; else rm -f "$t"; fi
  done
}

# Who holds the lock, from the holder file the running run wrote.
holder() {
  local pid since dir args now
  if [ -r "$HOLDER" ] && { read -r pid; read -r since; read -r dir; read -r args; } <"$HOLDER" && kill -0 "$pid" 2>/dev/null; then
    now=$(date +%s)
    echo "$(basename "$dir") (pid $pid, scripts/test.sh ${args:-(whole workspace)}) has held it for $(secs_label $((now - since)))"
  else
    echo "a run that didn't say who it is (an older test.sh) holds it"
  fi
}

started=$(date +%s) next=0
report() {
  local now names="" t
  now=$(date +%s)
  # A second early: the lock's wait comes back after REPORT_EVERY, not always past it.
  [ "$now" -ge $((next - 2)) ] || return 0
  next=$((now + REPORT_EVERY))
  for t in "$@"; do names="$names${names:+, }$(basename "$(head -n 1 "$t" 2>/dev/null || echo '?')")"; done
  echo "test.sh: waiting for the test lock $LOCK for $(secs_label $((now - started))): number $(($# + 1)) in line${names:+ after $names}; $(holder). Tests start once it's this run's turn; send this command to the background to work meanwhile." >&2
}

# The run whose turn it is takes its ticket out, says who holds the lock, then runs the tests.
# shellcheck disable=SC2016
run='rm -f "$1"; printf "%s\n%s\n%s\n%s\n" "$$" "$(date +%s)" "$2" "$3" >"$4"
[ "$5" = 0 ] || echo "test.sh: got the test lock after waiting $(( $(date +%s) - $5 ))s" >&2
shift 5; exec cargo test "$@"'
while :; do
  # shellcheck disable=SC2207
  before=($(ahead))
  if [ ${#before[@]} -gt 0 ]; then
    report "${before[@]}"
    sleep 2
    continue
  fi
  # First in line: wait on the lock itself, coming back each minute to say so.
  lockf -s -t 0 "$LOCK" true || report
  waited=0
  [ "$next" -eq 0 ] || waited=$started
  set +e
  lockf -s -t "$REPORT_EVERY" "$LOCK" /bin/bash -c "$run" test.sh "$ticket" "$ROOT" "$*" "$HOLDER" "$waited" ${scope[@]+"${scope[@]}"} "$@"
  rc=$?
  set -e
  # lockf's own "timed out" (EX_TEMPFAIL); cargo test never exits 75.
  [ "$rc" -eq 75 ] || exit "$rc"
done
