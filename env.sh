# Source before building: `. ./env.sh`
# libghostty-vt needs Zig 0.15.2 exactly and the MacOSX15.4 SDK (fakebin/xcrun shim).
_MIDNA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
export PATH="$_MIDNA_ROOT/.toolchain/fakebin:$_MIDNA_ROOT/.toolchain/zig:$PATH"
export GHOSTTY_SOURCE_DIR="$_MIDNA_ROOT/.toolchain/ghostty-src"
# Debug builds of libghostty are ~5000x slower; always optimize it.
export LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast
