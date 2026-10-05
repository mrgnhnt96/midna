#!/bin/bash
# OPT-IN: notarize and staple a built Midna.app. This UPLOADS the app to Apple, so nothing
# runs it automatically (not build-app.sh, not make-update.sh, not the e2e test).
#
#   packaging/notarize.sh dist/0.1.1/Midna.app
#
# One-time setup (stores an app-specific password in the keychain under a profile name):
#   xcrun notarytool store-credentials midna-notary --apple-id <you@example.com> --team-id U2G2XV3688
# Env: MIDNA_NOTARY_PROFILE (default midna-notary).
# CI (.github/workflows/release.yml) uses an App Store Connect API key instead of a profile:
#   MIDNA_NOTARY_KEY (path to the .p8), MIDNA_NOTARY_KEY_ID, MIDNA_NOTARY_ISSUER.
# Order for a release: build-app.sh -> notarize.sh -> make-update.sh (the archive must contain
# the stapled app, so the ticket travels with every update).
set -euo pipefail
APP="${1:?usage: notarize.sh path/to/Midna.app}"
PROFILE="${MIDNA_NOTARY_PROFILE:-midna-notary}"
XCRUN=/usr/bin/xcrun   # not the toolchain shim from env.sh
/usr/bin/codesign --verify --deep --strict "$APP"
if /usr/bin/codesign -dv "$APP" 2>&1 | grep -q 'Signature=adhoc'; then
  echo "notarize.sh: $APP is ad-hoc signed; notarization needs a Developer ID signature" >&2; exit 1
fi
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
ZIP="$TMP/Midna.zip"
/usr/bin/ditto -c -k --keepParent "$APP" "$ZIP"
echo "==> submitting to Apple notary service; this uploads the app"
if [ -n "${MIDNA_NOTARY_KEY:-}" ]; then
  AUTH=(--key "$MIDNA_NOTARY_KEY" --key-id "${MIDNA_NOTARY_KEY_ID:?}" --issuer "${MIDNA_NOTARY_ISSUER:?}")
else
  AUTH=(--keychain-profile "$PROFILE")
fi
"$XCRUN" notarytool submit "$ZIP" "${AUTH[@]}" --wait
"$XCRUN" stapler staple "$APP"
/usr/sbin/spctl -a -t exec -vv "$APP"
echo "==> notarized and stapled $APP"
