# Source before building: `. ./env.sh`
# libghostty-vt needs Zig 0.15.2 exactly and the MacOSX15.4 SDK (fakebin/xcrun shim).
_MIDNA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
# A git worktree uses the main checkout's toolchain.
_MIDNA_MAIN="$(git -C "$_MIDNA_ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"
_MIDNA_MAIN="${_MIDNA_MAIN%/.git}"
[ -d "$_MIDNA_MAIN/.toolchain" ] || _MIDNA_MAIN="$_MIDNA_ROOT"
export PATH="$_MIDNA_MAIN/.toolchain/fakebin:$_MIDNA_MAIN/.toolchain/zig:$PATH"
export GHOSTTY_SOURCE_DIR="$_MIDNA_MAIN/.toolchain/ghostty-src"
# Debug builds of libghostty are ~5000x slower; always optimize it.
export LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast
# Each checkout keeps its own target/. Never point a worktree's CARGO_TARGET_DIR or build dir
# at another checkout's: cargo names a path crate's artifacts by its path inside the
# workspace, so checkouts overwrite each other's midna crates and can link a stale one.
