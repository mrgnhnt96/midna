---
title: Build from source
description: Build midna yourself, run it in dev mode, and package a Midna.app.
---

Building midna yourself is for working on it. To just use midna, [download the DMG](/docs/install/).

## Requirements

- A Mac with Apple Silicon
- Xcode or the Command Line Tools
- [Rust](https://rustup.rs) (edition 2024)
- `git` and `curl`

The terminal engine (libghostty-vt) builds only with Zig 0.15.2 and a pinned ghostty source. A setup script fetches both.

## Build

```bash
git clone https://github.com/mrgnhnt96/midna.git
cd midna
./scripts/setup-toolchain.sh
. ./env.sh && cargo build
```

- `setup-toolchain.sh` downloads Zig 0.15.2 and the pinned ghostty source into `.toolchain/`. If your default macOS SDK is newer than Zig 0.15.2 can link against, it points it at an installed macOS 15 SDK instead. Running it again keeps what's already there.
- Always source `env.sh` before building. It puts the toolchain on your `PATH` and sets the build options the engine needs.

`cargo build` produces three binaries in `target/debug/`: `midnad` (the daemon), `midna` (the CLI) and `midna-app` (the GUI).

```bash
cargo test --workspace     # every test uses a temporary MIDNA_HOME
scripts/smoke.sh           # end-to-end check against a real temporary daemon
```

## Run in dev mode

Use a temporary home with a short path. Unix socket paths are limited to 104 bytes, and you don't want to touch your real midna data.

```bash
export MIDNA_HOME=$(mktemp -d /tmp/mh.XXXX)
MIDNA_APP_PATH=$PWD/target/debug/midna-app target/debug/midnad --foreground &
target/debug/midna-app                        # the GUI
target/debug/midna info                       # the CLI
target/debug/midna open -- /bin/zsh           # prints the new terminal's id
MIDNA_BACKEND=fake target/debug/midna-app     # the GUI with sample data, no daemon
```

`MIDNA_APP_PATH` tells a debug daemon which binary counts as the human; release builds ignore it. A dev build of the app registers nothing and never updates. If no daemon is answering, it starts the `midnad` next to it.

## Package a Midna.app

```bash
packaging/build-app.sh       # -> dist/<version>/Midna.app
```

The app is signed with the first Developer ID Application certificate in your keychain, or ad hoc if there isn't one. An ad-hoc build is a new app to macOS each time, so permissions like Accessibility don't carry over between builds. It also falls back to recognizing the app by name, which is weaker than the [signature check](/docs/security/#files-and-builds) signed builds get.

A build you make yourself doesn't carry midna's release update key, so it won't install updates from the official feed.

## Layout

| Crate | What it is |
| --- | --- |
| `midna-proto` | Shared types, the method catalog and OpenRPC, the settings catalog |
| `midnad` | The daemon: terminals, policy, triggers and webhooks, insights, in-place upgrade |
| `midna-cli` | The `midna` command, `midna mcp`, and the agent hook bridge |
| `midna-app` | The GPUI app: Midna.app |

The design notes are in `docs/`: `ARCHITECTURE.md` for the contract, `DECISIONS.md` for the reasoning, and `SECURITY.md` for the human/agent boundary.
