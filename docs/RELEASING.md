# Releasing midna

How a `Midna.app` is built, signed, published as an update, and installed. Code:
`packaging/`, `crates/midna-app/src/{install,updater,lifecycle}.rs`, and `midnad --launchd`
(`crates/midnad/src/main.rs`). Design decisions are in `docs/DECISIONS.md` ("Packaging,
install and auto-update").

## TL;DR: cut a release

```sh
./scripts/release.sh 0.2.0          # public release, from main
./scripts/release.sh 0.3.0-beta.1   # beta (prerelease), from any branch
```

`release.sh` checks the version is newer than the last one (`scripts/newest-version.py`), writes it
into `Cargo.toml`/`Cargo.lock` (`scripts/set-version.sh`), commits, tags `vX.Y.Z` and pushes.
The tag runs `.github/workflows/release.yml` on a `macos-15` runner, which:

1. checks the files' version matches the tag, sets up `.toolchain/` (`scripts/setup-toolchain.sh`)
2. imports the Developer ID certificate into a throwaway keychain
3. `packaging/build-app.sh` with the **release** update public key compiled in
4. `packaging/notarize.sh` (App Store Connect API key), staples
5. `scripts/build-dmg.sh` (branded window, `packaging/assets/dmg`), signs, notarizes and staples the DMG
6. release notes = this version's `CHANGELOG.md` section, else its `## Unreleased` section
7. `packaging/make-update.sh`: the `.app.tar.gz` and its signed feed entry
8. publishes the GitHub release (DMG + archive; betas as prereleases)
9. updates the feeds on the `channels` release: `stable.json` (public releases) and `beta.json`
   (newest of any; a stable hotfix older than the current beta leaves it alone)

Before tagging, rename `## Unreleased` in `CHANGELOG.md` to `## 0.2.0 — <date>`. The website
(`site/`, deployed by `.github/workflows/site.yml`) shows the changelog, and its `/download/` page
starts the DMG from the latest non-prerelease.

`.github/workflows/build-macos.yml` (manual) builds an ad-hoc signed app as a CI artifact
without publishing anything.

### Repository secrets and variables

| Name | Kind | What |
|---|---|---|
| `DEVELOPER_ID_CERTIFICATE` | secret | base64 of the Developer ID Application `.p12` |
| `DEVELOPER_ID_CERTIFICATE_PASSWORD` | secret | its password |
| `APP_STORE_CONNECT_API_KEY` | secret | contents of the `AuthKey_<id>.p8` (notarization) |
| `MIDNA_UPDATE_KEY` | secret | contents of `~/.config/midna-release/update-ed25519.key` |
| `DEVELOPER_ID_IDENTITY` | variable | `Developer ID Application: Morgan Hunt (U2G2XV3688)` |
| `APP_STORE_CONNECT_KEY_ID` / `APP_STORE_CONNECT_ISSUER_ID` | variable | the API key's ids |
| `MIDNA_UPDATE_PUBKEY` | variable | contents of `update-ed25519.pub`; the workflow checks it matches the secret |

### By hand (no CI)

```sh
export MIDNA_UPDATE_PUBKEY="$(cat ~/.config/midna-release/update-ed25519.pub)"
packaging/build-app.sh --version 0.2.0                   # -> dist/0.2.0/Midna.app (Developer ID signed)
packaging/notarize.sh dist/0.2.0/Midna.app               # OPT-IN: uploads to Apple, then staples
packaging/make-update.sh --app dist/0.2.0/Midna.app \
    --url-base https://github.com/mrgnhnt96/midna/releases/download/v0.2.0 --channel stable \
    --key ~/.config/midna-release/update-ed25519.key --notes "What changed"
```

`--version` overrides the workspace version for one build (every binary reports
`midna_proto::VERSION`, from `MIDNA_BUILD_VERSION`).

## The bundle (`packaging/build-app.sh`)

```
Midna.app/Contents/
  Info.plist                     id com.mrgnhnt.midna, version, LSMinimumSystemVersion 12.0,
                                 KassDictationHandshake, NSAccessibilityUsageDescription, …
  MacOS/midna-app                the GUI (CFBundleExecutable)
  MacOS/midnad                   the daemon (copied to MIDNA_HOME/bin on first launch)
  MacOS/midna                    the CLI (copied next to midnad)
  Library/LaunchAgents/com.mrgnhnt.midna.daemon.plist
  Resources/Midna.icns           PLACEHOLDER icon (packaging/make-icon.py draws it)
  Resources/Fonts/               Atkinson Hyperlegible Next + JetBrains Mono and their OFL
                                 licenses (the app embeds the same files with include_bytes!)
```

- Binaries are built with `cargo build --release` into `target/package` (separate from dev
  builds), `MACOSX_DEPLOYMENT_TARGET=12.0`, host architecture only. Universal builds
  (`lipo` of aarch64 + x86_64) are a TODO; the feed's `arch` field already allows them.
- **LaunchAgent plist**: `BundleProgram Contents/MacOS/midnad`, arguments `--launchd`,
  `RunAtLoad`, `KeepAlive {SuccessfulExit: false}`, `ProcessType Interactive`,
  `AssociatedBundleIdentifiers [com.mrgnhnt.midna]`, env `MIDNA_SIGTERM=stop`. launchd always
  starts the bundle's binary (SMAppService requires `BundleProgram`), and `--launchd` makes it
  exec the stable copy at `MIDNA_HOME/bin/current/midnad` right away (installing itself there
  if there is none). That's what `spikes/install-update` proved.
- **Signing** is inside-out: `midnad` (identifier `com.mrgnhnt.midna.daemon`), `midna`
  (`com.mrgnhnt.midna.cli`), then the bundle. Hardened runtime, secure timestamp,
  `packaging/entitlements.plist` (deliberately empty). The identity is the first "Developer ID
  Application" certificate in the keychain (team U2G2XV3688), else ad-hoc. `MIDNA_SIGN_IDENTITY`
  picks one, `--adhoc` forces ad-hoc, and `MIDNA_SIGN_TIMESTAMP=0` signs offline.
- **TCC**: grants (Accessibility) are keyed to the designated requirement, which is the same
  for every Developer ID build, so they survive updates. An ad-hoc build's requirement is its
  hash, so each ad-hoc build is a new app to TCC.

## Notarization (`packaging/notarize.sh`, opt-in)

Never run automatically: it uploads the app to Apple. One-time setup:
`xcrun notarytool store-credentials midna-notary --apple-id … --team-id U2G2XV3688`. Then
`packaging/notarize.sh dist/<v>/Midna.app` zips, submits with `--wait`, staples, and runs
`spctl`. Notarize **before** `make-update.sh` so the archive carries the stapled ticket.
Un-notarized Developer ID builds run fine for the developer; Gatekeeper blocks them on other
Macs when they're downloaded through a browser (quarantine). Updates fetched by midna itself
aren't quarantined.

## First launch (what the app does)

`crates/midna-app/src/lifecycle.rs` runs this on a background thread on every launch. It's
idempotent, so later launches only check:

1. **Install the daemon**: if `MIDNA_HOME/bin/current/midnad` isn't byte-identical to the
   bundled one, run `Contents/MacOS/midnad --install-self`. That copies `midnad` and `midna` to
   `bin/<version>-<hash>/` and swaps the `current` symlink atomically. Old versions are pruned
   to the current one plus two.
2. **Register the LaunchAgent**: `SMAppService.agent(plistName:)` on macOS 13+. If macOS says
   *requires approval*, Settings ▸ Permissions ▸ Login item shows "Needs your OK in Login Items"
   with **Fix** (`SMAppService.openSystemSettingsLoginItems()`), and the app polls until it's on.
   On macOS 12 it writes `~/Library/LaunchAgents/<label>.plist` (program = `bin/current/midnad`)
   and runs `launchctl bootstrap`.
3. **The CLI for agents**: symlink `~/.local/bin/midna` → `MIDNA_HOME/bin/current/midna` when
   `~/.local/bin` is on the login shell's `PATH` (read with `$SHELL -l -i -c 'echo $PATH'`). If
   it isn't, Settings offers "Install to ~/.local/bin" plus the `export PATH=…` line to add
   yourself. midna never edits shell rc files. A `midna` there that isn't our symlink is left
   alone. midna's own terminals always have the CLI dir on `PATH`.
4. **Connect**, and if the running daemon isn't `bin/current/midnad` (compared through
   `daemon.info.binary`), call `daemon.upgrade` with it. That's a same-PID re-exec, and the shells
   survive. If the agent is enabled but not answering after about 4s, `launchctl kickstart`.

**Dev mode** (`MIDNA_DEV=1`, or running outside a `.app`, e.g. `target/debug/midna-app`)
skips all of that. If midnad isn't answering, it starts the `midnad` next to the binary
(`MIDNA_NO_SPAWN=1` turns that off). `MIDNA_BACKEND=fake` does nothing at all.

**Uninstall**: `Midna.app/Contents/MacOS/midna-app --uninstall` runs `daemon.stop` (which hangs
up every terminal), unregisters the login item and removes our CLI link. It leaves
`MIDNA_HOME` in place. `--login-item-status` prints the SMAppService state.

## Auto-update

**Feed**: the setting `updates.feed_url` (human only; default
`https://github.com/mrgnhnt96/midna/releases/download/channels/{channel}.json`, where `{channel}`
is replaced by `updates.channel`, `stable` or `beta`; `midna_proto::settings::DEFAULT_FEED_URL`). `MIDNA_UPDATE_FEED_URL` overrides it (tests). The feed is one JSON object:

```json
{
  "version": "0.1.1",
  "pub_date": "2026-10-03T21:40:00Z",
  "notes": "What changed (shown in Settings)",
  "url": "https://github.com/mrgnhnt96/midna/releases/download/v0.1.1/Midna-0.1.1.app.tar.gz",
  "sha256": "9f2c…(64 hex)",
  "size": 31457280,
  "minimum_macos": "12.0",
  "arch": "aarch64",
  "signature": "base64 ed25519 signature"
}
```

- `signature` is ed25519 over exactly
  `"midna-update-v1\nversion=<version>\nsha256=<lowercase sha256>\nminimum_macos=<min>\narch=<arch>\n"`.
  The public key (base64, raw 32 bytes) is compiled in from `MIDNA_UPDATE_PUBKEY` at build
  time. A build without a key never updates (Settings says so). `url`, `notes` and `pub_date`
  aren't signed: the signed sha256 pins the bytes, so a mirror can serve the file.
- `arch`: `aarch64`, `x86_64` or `universal` (default). `minimum_macos` defaults to `12.0`.
- Archive: `.tar.gz` (or `.zip`) containing `Midna.app` at the top level.

**Client flow** (`updater.rs`). It runs about 5s after launch, then every 6h (Settings has
"Check now"):

1. Fetch the feed, verify the signature, and skip unless the version is newer by semver
   (pre-releases sort first; there are no downgrades). The arch and macOS version must fit.
2. Download to `MIDNA_HOME/updates/<v>/` while hashing; on a sha256 mismatch, drop it.
3. Extract with `tar` / `ditto`, then check that `Midna.app` has the same bundle id, has the
   feed's version, passes `codesign --verify --deep --strict`, and has the **same Team ID** as
   the running app (when that one is Developer ID signed).
4. State becomes **Ready**: the status bar shows "↻ Update ready (v) · restart to apply" and
   Settings ▸ Updates shows "Restart to apply". Quitting midna with an update ready also
   installs it (no relaunch).
5. **Apply**: the staged app is moved next to the installed one (a `ditto` copy if it's on
   another volume) and exchanged with `renamex_np(…, RENAME_SWAP)`. That's a single atomic
   swap, so `Midna.app` is always the complete old or the complete new bundle. Volumes without
   swap support fall back to two renames with rollback. Then the old bundle is deleted, a
   detached helper waits for this process to exit and runs `open Midna.app`, and the app quits.
6. The new app's launch path (above) installs its midnad into `bin/` and `daemon.upgrade`s the
   running daemon in place. Terminals keep running, with the same session ids, pids and screens
   (`crates/midnad/tests/upgrade.rs`, and the e2e test below).

Debug: `MIDNA_UPDATE_CHECK_SECS=N` changes the interval, and `MIDNA_DEBUG_UPDATE=apply`
applies as soon as an update is ready. The app logs to `MIDNA_HOME/app.log`, the daemon to
`MIDNA_HOME/midnad.log`.

## Keys

- **Dev/test**: `packaging/gen-update-key.sh` writes `~/.config/midna-dev/update-ed25519.key`
  (PKCS#8, mode 0600) and `.pub`. `build-app.sh` embeds that `.pub` when `MIDNA_UPDATE_PUBKEY`
  isn't set, and `make-update.sh` signs with that `.key` unless `--key` / `MIDNA_UPDATE_KEY`
  says otherwise.
- **Release**: generate a separate key the same way and keep the secret half off the dev
  machine (CI secret). Builds embed only the public half, so the release key and the dev key
  are not interchangeable. An app built with the dev key only accepts dev-signed updates.
- `*.key` is in `.gitignore`. `packaging/release-tool` (`midna-release keygen|pubkey|feed|verify`)
  is the only code that touches secret keys.

## End-to-end test

`packaging/e2e/update-e2e.sh` (about 6 min, most of it the two release builds; `--no-build`
reuses them, `--keep` skips deleting the installs):

- builds **test-flavored** v0.1.0 and v0.1.1: bundle id `com.mrgnhnt.midna.test`, label
  `com.mrgnhnt.midna.test.daemon`, and `MIDNA_HOME=/tmp/mdt-e2e` in both `LSEnvironment` and
  the LaunchAgent's `EnvironmentVariables`, so the real install and its TCC grants are never
  touched
- installs v0.1.0 to `~/Applications/MidnaTest/Midna.app` and launches it; SMAppService
  registers the test label
- opens a `zsh -f` session and runs a command in it
- publishes v0.1.1 with `make-update.sh` on `python3 -m http.server` (127.0.0.1)
- waits for the swap, the relaunch at 0.1.1, and the daemon re-exec at 0.1.1, then asserts the
  same daemon pid, the same session id and shell pid, the screen kept, and the shell still
  interactive
- cleans up: quits the app, runs `--uninstall` (daemon.stop + unregister), boots out the label
  as a fallback, deletes the install, the test home and the LaunchServices registration, and
  stops the server

## Checklist

- [ ] `CHANGELOG.md`: `## Unreleased` renamed to the version; `cargo test` green
- [ ] `./scripts/release.sh <version>` (a beta first for anything risky)
- [ ] the Release workflow is green; the DMG opens without a Gatekeeper warning on another Mac
- [ ] an installed copy on that channel picks the update up (Settings › Updates › Check now)
