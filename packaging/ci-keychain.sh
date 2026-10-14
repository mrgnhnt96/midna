#!/bin/bash
# CI only (.github/workflows/release.yml): put the Developer ID certificate in a
# keychain of its own that lives only for this job, or delete that keychain.
#
#   CERTIFICATE=<base64 .p12> CERTIFICATE_PASSWORD=… packaging/ci-keychain.sh import
#   (CERTIFICATE_PASSWORD may be empty or unset: the release .p12 has no password)
#   packaging/ci-keychain.sh delete
set -euo pipefail
keychain="${RUNNER_TEMP:?run this on a GitHub runner}/signing.keychain-db"
case "${1:-}" in
  import)
    password=$(openssl rand -hex 24)
    echo "${CERTIFICATE:?}" | base64 --decode > "$RUNNER_TEMP/certificate.p12"
    security create-keychain -p "$password" "$keychain"
    security set-keychain-settings -lut 21600 "$keychain"
    security unlock-keychain -p "$password" "$keychain"
    security import "$RUNNER_TEMP/certificate.p12" -k "$keychain" -P "${CERTIFICATE_PASSWORD-}" -T /usr/bin/codesign
    security set-key-partition-list -S apple-tool:,apple: -s -k "$password" "$keychain" >/dev/null
    security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
    rm "$RUNNER_TEMP/certificate.p12"
    security find-identity -v -p codesigning "$keychain"
    ;;
  delete)
    security delete-keychain "$keychain" 2>/dev/null || true
    ;;
  *)
    echo "usage: ci-keychain.sh import|delete" >&2
    exit 2
    ;;
esac
