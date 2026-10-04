#!/bin/bash
# Generate an ed25519 update-signing keypair for DEVELOPMENT / testing.
#   packaging/gen-update-key.sh [dir]     (default ~/.config/midna-dev)
# Writes <dir>/update-ed25519.key (PKCS#8, mode 0600; never commit it) and
# <dir>/update-ed25519.pub (base64 raw public key, embedded by build-app.sh).
# The release key is made the same way but kept offline / in CI secrets (docs/RELEASING.md).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="${1:-$HOME/.config/midna-dev}"
mkdir -p "$DIR" && chmod 700 "$DIR"
if [ -f "$DIR/update-ed25519.key" ]; then
  echo "$DIR/update-ed25519.key already exists; public key:"; cat "$DIR/update-ed25519.pub"; exit 0
fi
cargo build --release -q --manifest-path "$ROOT/packaging/release-tool/Cargo.toml"
"$ROOT/packaging/release-tool/target/release/midna-release" keygen "$DIR/update-ed25519.key" > "$DIR/update-ed25519.pub"
echo "wrote $DIR/update-ed25519.key (secret) and $DIR/update-ed25519.pub:"
cat "$DIR/update-ed25519.pub"
