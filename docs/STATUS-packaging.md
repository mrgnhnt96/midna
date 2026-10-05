# Packaging / install / auto-update status

The full how-to is in `docs/RELEASING.md`; decisions are in `docs/DECISIONS.md` ("Packaging, install and auto-update").

## How to use

```sh
packaging/gen-update-key.sh                 # dev ed25519 keypair -> ~/.config/midna-dev/ (done once; exists now)
packaging/build-app.sh                      # -> dist/0.1.0/Midna.app, Developer ID signed (U2G2XV3688)
packaging/make-update.sh --app dist/0.1.0/Midna.app --url-base http://127.0.0.1:8791   # archive + signed feed
packaging/notarize.sh dist/0.1.0/Midna.app  # OPT-IN, uploads to Apple; never run by anything else
packaging/e2e/update-e2e.sh [--no-build]    # full install + update test with a test bundle id/label/home
Midna.app/Contents/MacOS/midna-app --uninstall | --login-item-status
```

`dist/0.1.0/Midna.app` is built: 28 MB, hardened runtime, Developer ID, unnotarized (Gatekeeper says "Unnotarized Developer ID"). It embeds the **dev** update key.

## What works (verified)

- **`build-app.sh`**:
  - builds release binaries in `target/package`, with min macOS 12.0 (`minos 12.0` checked with otool)
  - assembles `MacOS/{midna-app,midnad,midna}`, the LaunchAgent plist, an Info.plist with every requested key, the placeholder `.icns` and the fonts with their OFL texts
  - signs inside-out with Developer ID, or ad-hoc with `--adhoc` or when there's no certificate (both branches tested)
  - `--version` sets one version for every binary (`midna_proto::VERSION`)
- **First launch.**
  - `midnad --install-self` into `bin/<v>-<hash>`.
  - SMAppService `agentServiceWithPlistName` is registered and **Enabled** without a prompt.
  - launchd starts the bundle's midnad, and `--launchd` hands off to `bin/current` (`daemon.info.binary` = `…/bin/0.1.0-<hash>/midnad`).
  - The CLI is symlinked.
  - The app connects.
- **Auto-update, end to end (`update-e2e.sh`: 20/20 checks pass):**
  - v0.1.0 is installed to `~/Applications/MidnaTest/Midna.app`, and v0.1.1 is published on a 127.0.0.1 feed.
  - The app finds it about 4s later; it downloads, checks sha256 and the ed25519 signature, extracts, then checks the codesign and Team ID.
  - It swaps with `RENAME_SWAP`, quits, and is relaunched as 0.1.1 (1s).
  - The new app runs `daemon.upgrade` and midnad re-execs as 0.1.1 with the **same pid**.
  - The shell session keeps its **same id and shell pid**, its screen is intact, and it still runs commands.
  - Exactly one app process remains and no staging is left behind.
  - Cleanup: the app quit, `--uninstall` (daemon.stop + unregister), the label is not loaded, the installs, test home and LaunchServices entry are deleted, and the server stopped.
  - Verified afterwards: no processes, no job, no files.
- **Dev mode**: `target/debug/midna-app` with a dead socket starts the sibling midnad (verified with a temp `MIDNA_HOME`).
- **Tests**:
  - `cargo test -p midna-app` passes 38, including new ones: signature round trip and tamper (version and sha bound, url free), semver, the atomic swap, bundle detection, PATH membership, and a CLI link that never overwrites foreign files.
  - `midna-proto` and the midnad lib pass, and so do `tests/upgrade.rs` (7) and `tests/daemon.rs` (20).
- **UI**:
  - The status bar shows "↻ Update ready (v) · restart to apply" (click applies).
  - Settings ▸ Updates has Update (Check now / Restart to apply / failure text) and Feed.
  - Permissions ▸ Login item comes from SMAppService (Fix opens Login Items, or re-registers), and Accessibility offers Fix, then Relaunch.
  - Agents ▸ midna CLI shows "Install to ~/.local/bin" with the PATH line when it isn't on PATH.

## Changes outside the new modules (small, additive)

- `midna-proto`: `VERSION` const; `DaemonInfo.binary`; setting `updates.feed_url` (human only).
- `midnad`: `--launchd` mode (`main.rs`); `install::running_binary`; `daemon.info` fills `binary`; `CARGO_PKG_VERSION` → `midna_proto::VERSION`.
- `midna-app`: `main.rs` (mods, `lifecycle::start`, headless args); `statusbar.rs` (update item); `settings_window.rs` (rows, `Act::Life` / `Act::AxFix`); `Cargo.toml` (+objc2-service-management, ureq, ring, base64, semver).
- `.gitignore`: `*.key`, `packaging/release-tool/target`, `packaging/e2e/work`.

## Not done / gaps

- The UI rows and the status-bar item were **not visually verified**. No screenshot was taken, because the screen was in use and full-screen captures are off limits. The logic was exercised through the e2e run with `MIDNA_DEBUG_UPDATE=apply`.
- No universal binary (the build is arm64 only; the feed's `arch` handles it).
- The macOS 12 legacy LaunchAgent path is untested.
- The `RequiresApproval` flow is untested: registration went straight to Enabled.
- Notarization was never run (opt-in by design).
- `midna update …` for agents isn't built; it needs an RPC that reaches the GUI.
- ~~The real feed host doesn't exist.~~ done: the feeds live on the `channels` GitHub release (docs/RELEASING.md).
- The icon is a placeholder.
- `daemon.upgraded.from.binary` shows the unresolved `bin/current` path (cosmetic, upgrade.rs).
- The release key isn't made: `dist/0.1.0` embeds the dev key, so make a release key before shipping (RELEASING.md).
- `packaging/e2e/work/{0.1.0,0.1.1}` keeps the test bundles (gitignored) for `--no-build` reruns.
