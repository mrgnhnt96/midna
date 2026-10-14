#!/bin/bash
# Run a command until it succeeds, up to N attempts, waiting longer after
# each failure (15s, 30s, 60s, …).
#
#   ./scripts/retry.sh 4 xcrun stapler staple Midna.dmg
#
# For the release's calls to Apple (notarytool, stapler), which now and then
# fail on a dropped connection rather than on anything wrong with the app.
set -uo pipefail
attempts="${1:?usage: retry.sh <attempts> <command…>}"
shift
delay=15
for ((i = 1; ; i++)); do
  "$@" && exit 0
  status=$?
  if ((i >= attempts)); then
    echo "retry.sh: failed $attempts times (exit $status): $*" >&2
    exit "$status"
  fi
  echo "retry.sh: attempt $i of $attempts failed (exit $status); trying again in ${delay}s" >&2
  sleep "$delay"
  delay=$((delay * 2))
done
