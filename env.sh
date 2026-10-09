# Source before building: `. ./env.sh`
# libghostty-vt needs Zig 0.15.2 exactly and the MacOSX15.4 SDK (fakebin/xcrun shim).
_MIDNA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
# A git worktree uses the main checkout's toolchain and compiled crates.
_MIDNA_MAIN="$(git -C "$_MIDNA_ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"
_MIDNA_MAIN="${_MIDNA_MAIN%/.git}"
[ -d "$_MIDNA_MAIN/.toolchain" ] || _MIDNA_MAIN="$_MIDNA_ROOT"
export PATH="$_MIDNA_MAIN/.toolchain/fakebin:$_MIDNA_MAIN/.toolchain/zig:$PATH"
export GHOSTTY_SOURCE_DIR="$_MIDNA_MAIN/.toolchain/ghostty-src"
# Debug builds of libghostty are ~5000x slower; always optimize it.
export LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast
# Every checkout builds into one shared build dir (cargo's lock lets one build run at a time),
# so a new worktree reuses the compiled dependencies. Linked binaries still land in this
# checkout's own target/ (or CARGO_TARGET_DIR), so target/debug/midna is always this tree's.
export CARGO_BUILD_BUILD_DIR="${CARGO_BUILD_BUILD_DIR:-$_MIDNA_MAIN/target}"
