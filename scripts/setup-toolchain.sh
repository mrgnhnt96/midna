#!/bin/bash
# Set up .toolchain/, the pinned build tools env.sh puts on PATH:
#
#   .toolchain/zig/            Zig 0.15.2, the only version libghostty-vt builds with
#   .toolchain/ghostty-src/    ghostty at the commit libghostty-vt-sys pins
#   .toolchain/fakebin/xcrun   only when the default SDK is newer than Zig 0.15.2 links
#                              against: answers SDK questions with a MacOSX15 SDK
#
#   ./scripts/setup-toolchain.sh
#
# Idempotent: what's already there is kept. CI runs it before every build; on a
# Mac you can copy .toolchain/ from another checkout instead.
set -euo pipefail
cd "$(dirname "$0")/.."

zig_version=0.15.2
# The commit libghostty-vt-sys's build.rs pins (GHOSTTY_COMMIT); update both together.
ghostty_commit=a887df42c56f6de86c0fe6da9c4eeca37931e083
dir=.toolchain
mkdir -p "$dir"

if [ "$("$dir/zig/zig" version 2>/dev/null)" != "$zig_version" ]; then
  arch=$(uname -m | sed 's/arm64/aarch64/')
  echo "==> Zig $zig_version ($arch)"
  rm -rf "$dir/zig"
  tmp=$(mktemp -d)
  curl -fsSL "https://ziglang.org/download/$zig_version/zig-$arch-macos-$zig_version.tar.xz" -o "$tmp/zig.tar.xz"
  mkdir -p "$dir/zig"
  tar -xJf "$tmp/zig.tar.xz" -C "$dir/zig" --strip-components 1
  rm -rf "$tmp"
fi

if [ "$(cat "$dir/ghostty-src/.ghostty-commit" 2>/dev/null)" != "$ghostty_commit" ]; then
  echo "==> ghostty $ghostty_commit"
  rm -rf "$dir/ghostty-src"
  git clone -q --filter=blob:none --no-checkout https://github.com/ghostty-org/ghostty.git "$dir/ghostty-src"
  git -C "$dir/ghostty-src" checkout -q "$ghostty_commit"
  echo "$ghostty_commit" > "$dir/ghostty-src/.ghostty-commit"
fi

# Zig 0.15.2 can't link against the macOS 26 SDK. When that's the default,
# point SDK lookups at a MacOSX15 SDK and pass everything else through.
sdk_major=$(/usr/bin/xcrun --show-sdk-version | cut -d. -f1)
if [ "$sdk_major" -gt 15 ]; then
  sdk=$(ls -d /Library/Developer/CommandLineTools/SDKs/MacOSX15.*.sdk \
    /Applications/Xcode*.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX15.*.sdk 2>/dev/null | sort -V | tail -1 || true)
  [ -n "$sdk" ] || { echo "The default SDK is $sdk_major and no MacOSX15 SDK is installed; Zig $zig_version needs one." >&2; exit 1; }
  version=$(basename "$sdk" .sdk | sed 's/^MacOSX//')
  mkdir -p "$dir/fakebin"
  cat > "$dir/fakebin/xcrun" <<EOF
#!/bin/sh
case "\$*" in *show-sdk-path*) echo $sdk; exit 0;; *show-sdk-version*) echo $version; exit 0;; esac
exec /usr/bin/xcrun "\$@"
EOF
  chmod +x "$dir/fakebin/xcrun"
else
  rm -rf "$dir/fakebin"
fi

echo "Toolchain ready. Source it before building: . ./env.sh"
