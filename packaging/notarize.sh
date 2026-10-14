#!/bin/bash
# OPT-IN: notarize and staple a built Midna.app or its DMG. This UPLOADS it to Apple, so
# nothing runs it automatically (not build-app.sh, not make-update.sh, not the e2e test).
#
#   packaging/notarize.sh dist/0.1.1/Midna.app
#   packaging/notarize.sh Midna-0.1.1-macos-arm64.dmg     (sign the DMG first)
#
# One-time setup (stores an app-specific password in the keychain under a profile name):
#   xcrun notarytool store-credentials midna-notary --apple-id <you@example.com> --team-id U2G2XV3688
# Env: MIDNA_NOTARY_PROFILE (default midna-notary).
# CI (.github/workflows/release.yml) uses an App Store Connect API key instead of a profile:
#   MIDNA_NOTARY_KEY (path to the .p8), MIDNA_NOTARY_KEY_ID, MIDNA_NOTARY_ISSUER.
# Order for a release: build-app.sh -> notarize.sh -> make-update.sh (the archive must contain
# the stapled app, so the ticket travels with every update).
# Apple's service drops connections now and then, so submitting and stapling are retried
# (MIDNA_NOTARY_ATTEMPTS, default 4).
set -euo pipefail
TARGET="${1:?usage: notarize.sh path/to/Midna.app|path/to/Midna.dmg}"
PROFILE="${MIDNA_NOTARY_PROFILE:-midna-notary}"
ATTEMPTS="${MIDNA_NOTARY_ATTEMPTS:-4}"
RETRY="$(cd "$(dirname "$0")/.." && pwd)/scripts/retry.sh"
XCRUN=/usr/bin/xcrun   # not the toolchain shim from env.sh
/usr/bin/codesign --verify --deep --strict "$TARGET"
if /usr/bin/codesign -dv "$TARGET" 2>&1 | grep -q 'Signature=adhoc'; then
  echo "notarize.sh: $TARGET is ad-hoc signed; notarization needs a Developer ID signature" >&2; exit 1
fi
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
if [[ "$TARGET" == *.dmg ]]; then
  UPLOAD="$TARGET"
else
  UPLOAD="$TMP/Midna.zip"
  /usr/bin/ditto -c -k --keepParent "$TARGET" "$UPLOAD"
fi
echo "==> submitting to Apple notary service; this uploads $(basename "$TARGET")"
if [ -n "${MIDNA_NOTARY_KEY:-}" ]; then
  AUTH=(--key "$MIDNA_NOTARY_KEY" --key-id "${MIDNA_NOTARY_KEY_ID:?}" --issuer "${MIDNA_NOTARY_ISSUER:?}")
else
  AUTH=(--keychain-profile "$PROFILE")
fi
"$RETRY" "$ATTEMPTS" "$XCRUN" notarytool submit "$UPLOAD" "${AUTH[@]}" --wait
"$RETRY" "$ATTEMPTS" "$XCRUN" stapler staple "$TARGET"
if [[ "$TARGET" == *.dmg ]]; then
  /usr/sbin/spctl --assess --type open --context context:primary-signature --verbose=2 "$TARGET"
else
  /usr/sbin/spctl -a -t exec -vv "$TARGET"
fi
echo "==> notarized and stapled $TARGET"
