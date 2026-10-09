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
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck disable=SC1091
. "$ROOT/env.sh"
cd "$ROOT"
LOCK="${MIDNA_TEST_LOCK:-/tmp/midna-tests.lock}"

scope=(--workspace) cargo_args=()
for a in "$@"; do
  [ "$a" = "--" ] && break
  case "$a" in -p|--package|--package=*|-p?*|--workspace) scope=() ;; esac
  cargo_args+=("$a")
done

nice -n 10 cargo test ${scope[@]+"${scope[@]}"} ${cargo_args[@]+"${cargo_args[@]}"} --no-run
if ! lockf -s -t 0 "$LOCK" true; then
  echo "test.sh: another checkout's tests are running; waiting for $LOCK" >&2
fi
exec lockf -s "$LOCK" cargo test ${scope[@]+"${scope[@]}"} "$@"
