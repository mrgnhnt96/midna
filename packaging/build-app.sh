#!/bin/bash
# Build release binaries and assemble a signed Midna.app. See docs/RELEASING.md.
#
#   packaging/build-app.sh [options]
#     --version V         version for the bundle and every binary (default: workspace Cargo version)
#     --out DIR           output directory (default: dist/<version>); the app is DIR/Midna.app
#     --adhoc             ad-hoc sign even when a Developer ID certificate is available
#     --no-build          reuse the binaries from the last build (same --target-dir)
#     --target-dir DIR    cargo target dir (default: target/package, kept apart from dev builds)
#   Test flavors (used by packaging/e2e/update-e2e.sh; never for releases):
#     --bundle-id ID      CFBundleIdentifier (default com.mrgnhnt.midna)
#     --label LABEL       LaunchAgent label (default com.mrgnhnt.midna.daemon)
#     --env K=V           add K=V to the app's LSEnvironment AND the daemon's EnvironmentVariables
#                         (e.g. MIDNA_HOME=/tmp/x); repeatable
#     --name NAME         CFBundleName / CFBundleDisplayName (default Midna)
#     --icon FILE         .icns to ship (default packaging/assets/Midna.icns)
#     --no-update-key     embed no update key: the build never updates itself
#     --dev-home DIR      build Midna Dev: every binary always uses DIR as its home (MIDNA_HOME and
#                         MIDNA_SOCKET are ignored), never updates, and refuses to write the
#                         installed Midna's files (midna_proto::paths::guard_write)
#     --dev-drivers       honour the MIDNA_DEBUG_* / updater-override env vars (cargo feature
#                         midna-app/dev-drivers). Test bundles only: those drivers act as the human.
#
# Env: MIDNA_UPDATE_PUBKEY (base64 ed25519 public key; default ~/.config/midna-dev/update-ed25519.pub),
#      MIDNA_SIGN_IDENTITY (codesign identity; default the first "Developer ID Application" one),
#      MIDNA_SIGN_TIMESTAMP=0 (sign without Apple's timestamp server, e.g. offline).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="" OUT="" ADHOC=0 BUILD=1 TARGET_DIR="$ROOT/target/package"
BUNDLE_ID="com.mrgnhnt.midna" LABEL="com.mrgnhnt.midna.daemon" NAME="Midna" ICON="" UPDATE_KEY=1 EXTRA_ENV=() FEATURES=() DEV_HOME=""
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --adhoc) ADHOC=1; shift ;;
    --no-build) BUILD=0; shift ;;
    --target-dir) TARGET_DIR="$2"; shift 2 ;;
    --bundle-id) BUNDLE_ID="$2"; shift 2 ;;
    --label) LABEL="$2"; shift 2 ;;
    --name) NAME="$2"; shift 2 ;;
    --icon) ICON="$2"; shift 2 ;;
    --no-update-key) UPDATE_KEY=0; shift ;;
    --env) EXTRA_ENV+=("$2"); shift 2 ;;
    --dev-home) DEV_HOME="$2"; shift 2 ;;
    --dev-drivers) FEATURES=(--features midna-app/dev-drivers); shift ;;
    -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
    *) echo "build-app.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

if [ -z "$VERSION" ]; then
  VERSION="$(awk '/^\[workspace.package\]/{p=1} p && /^version *=/{gsub(/[" ]/,"",$0); sub(/version=/,""); print; exit}' Cargo.toml)"
fi
OUT="${OUT:-$ROOT/dist/$VERSION}"
if [ -n "$DEV_HOME" ]; then
  case "$DEV_HOME" in /*) ;; *) echo "build-app.sh: --dev-home must be absolute" >&2; exit 2 ;; esac
  case "${DEV_HOME%/}/" in "$HOME/Library/Application Support/com.mrgnhnt.midna/"*)
    echo "build-app.sh: --dev-home can't be the installed Midna's home" >&2; exit 2 ;;
  esac
  [ "$UPDATE_KEY" = 0 ] || { echo "build-app.sh: --dev-home needs --no-update-key" >&2; exit 2; }
fi
case "$OUT" in /*) ;; *) OUT="$ROOT/$OUT" ;; esac
APP="$OUT/Midna.app"

PUBKEY="${MIDNA_UPDATE_PUBKEY:-}"
[ "$UPDATE_KEY" = 1 ] || PUBKEY=""
if [ -z "$PUBKEY" ] && [ "$UPDATE_KEY" = 1 ] && [ -f "$HOME/.config/midna-dev/update-ed25519.pub" ]; then
  PUBKEY="$(tr -d '[:space:]' < "$HOME/.config/midna-dev/update-ed25519.pub")"
fi
[ -n "$PUBKEY" ] || echo "build-app.sh: no update key (MIDNA_UPDATE_PUBKEY or ~/.config/midna-dev/update-ed25519.pub): this build will never update itself" >&2

# ---------------------------------------------------------------- build
BIN="$TARGET_DIR/release"
if [ "$BUILD" = 1 ]; then
  # shellcheck disable=SC1091
  . "$ROOT/env.sh"
  echo "==> cargo build --release (midna $VERSION, target $TARGET_DIR)"
  if [ -n "$PUBKEY" ]; then export MIDNA_UPDATE_PUBKEY="$PUBKEY"; else unset MIDNA_UPDATE_PUBKEY; fi
  MIDNA_DEV_HOME="$DEV_HOME" MIDNA_BUILD_VERSION="$VERSION" MACOSX_DEPLOYMENT_TARGET=12.0 CARGO_TARGET_DIR="$TARGET_DIR" CARGO_BUILD_BUILD_DIR="$TARGET_DIR" \
    cargo build --release -p midna-app -p midnad -p midna-cli --bin midna-app --bin midnad --bin midna ${FEATURES[@]+"${FEATURES[@]}"}
fi
for b in midna-app midnad midna; do [ -x "$BIN/$b" ] || { echo "missing $BIN/$b" >&2; exit 1; }; done

# ---------------------------------------------------------------- assemble
echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/Fonts" "$APP/Contents/Library/LaunchAgents"
cp "$BIN/midna-app" "$BIN/midnad" "$BIN/midna" "$APP/Contents/MacOS/"
if [ -n "$ICON" ]; then
  cp "$ICON" "$APP/Contents/Resources/Midna.icns"
else
  [ -f packaging/assets/Midna.icns ] || python3 packaging/make-icon.py packaging/assets/Midna.icns
  cp packaging/assets/Midna.icns "$APP/Contents/Resources/Midna.icns"
fi
# The fonts are compiled into midna-app (include_bytes!); the files and their OFL licenses
# ship too so the licenses travel with the binary.
cp crates/midna-app/assets/fonts/* "$APP/Contents/Resources/Fonts/"

# Finder's "Open in Midna" (packaging/finder-sync). /usr/bin/xcrun, not env.sh's SDK shim.
APPEX="$APP/Contents/PlugIns/MidnaFinderSync.appex"
mkdir -p "$APPEX/Contents/MacOS"
/usr/bin/xcrun --sdk macosx swiftc -O -module-name MidnaFinderSync -parse-as-library -application-extension \
  -target "$(uname -m)-apple-macos12.0" -framework FinderSync -Xlinker -e -Xlinker _NSExtensionMain \
  -o "$APPEX/Contents/MacOS/MidnaFinderSync" packaging/finder-sync/FinderSync.swift

env_dict() {  # <dict> body for EXTRA_ENV (+ any fixed pairs given as args)
  local kv
  for kv in "$@" ${EXTRA_ENV[@]+"${EXTRA_ENV[@]}"}; do
    printf '    <key>%s</key><string>%s</string>\n' "${kv%%=*}" "${kv#*=}"
  done
}

LSENV=""
if [ ${#EXTRA_ENV[@]} -gt 0 ]; then
  LSENV="  <key>LSEnvironment</key>
  <dict>
$(env_dict)
  </dict>"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleName</key><string>$NAME</string>
  <key>CFBundleDisplayName</key><string>$NAME</string>
  <key>CFBundleExecutable</key><string>midna-app</string>
  <key>CFBundleIconFile</key><string>Midna</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>$BUNDLE_ID</string>
      <key>CFBundleURLSchemes</key><array><string>$BUNDLE_ID</string></array>
    </dict>
  </array>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Folder</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key><array><string>public.folder</string></array>
    </dict>
  </array>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSHumanReadableCopyright</key><string>© Morgan Hunt</string>
  <key>NSAccessibilityUsageDescription</key><string>Midna uses Accessibility so Kass dictation can read and edit what you type in a terminal, and so agents can arrange midna's windows when you allow it.</string>
  <key>NSAppleEventsUsageDescription</key><string>Programs you run in midna terminals may ask to control other apps; macOS asks on their behalf.</string>
  <key>KassDictationHandshake</key><true/>
  <key>MidnaDaemonLabel</key><string>$LABEL</string>
$LSENV
</dict>
</plist>
PLIST

cat > "$APPEX/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID.finder-sync</string>
  <key>CFBundleName</key><string>MidnaFinderSync</string>
  <key>CFBundleDisplayName</key><string>$NAME</string>
  <key>CFBundleExecutable</key><string>MidnaFinderSync</string>
  <key>CFBundlePackageType</key><string>XPC!</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSExtension</key>
  <dict>
    <key>NSExtensionPointIdentifier</key><string>com.apple.FinderSync</string>
    <key>NSExtensionPrincipalClass</key><string>MidnaFinderSync.FinderSync</string>
    <key>NSExtensionAttributes</key><dict/>
  </dict>
</dict>
</plist>
PLIST

# SMAppService runs the bundle's midnad (BundleProgram); `--launchd` makes it hand off to the
# stable copy in MIDNA_HOME/bin/current (proven in spikes/install-update). MIDNA_SIGTERM=stop:
# launchd's SIGTERM (unregister, logout) stops midnad instead of re-exec'ing it.
cat > "$APP/Contents/Library/LaunchAgents/$LABEL.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>BundleProgram</key><string>Contents/MacOS/midnad</string>
  <key>ProgramArguments</key>
  <array><string>Contents/MacOS/midnad</string><string>--launchd</string></array>
  <key>AssociatedBundleIdentifiers</key><array><string>$BUNDLE_ID</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>EnvironmentVariables</key>
  <dict>
$(env_dict MIDNA_SIGTERM=stop)
  </dict>
</dict>
</plist>
PLIST
plutil -lint -s "$APP/Contents/Info.plist" "$APP/Contents/Library/LaunchAgents/$LABEL.plist" "$APPEX/Contents/Info.plist"

# ---------------------------------------------------------------- sign (inside out)
IDENTITY="${MIDNA_SIGN_IDENTITY:-}"
if [ -z "$IDENTITY" ] && [ "$ADHOC" = 0 ]; then
  IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)"
fi
[ -n "$IDENTITY" ] || IDENTITY="-"
SIGN=(/usr/bin/codesign --force --sign "$IDENTITY" --options runtime)
if [ "$IDENTITY" != "-" ] && [ "${MIDNA_SIGN_TIMESTAMP:-1}" != 0 ]; then SIGN+=(--timestamp); else SIGN+=(--timestamp=none); fi
"${SIGN[@]}" --entitlements packaging/finder-sync/entitlements.plist "$APPEX"
SIGN+=(--entitlements packaging/entitlements.plist)
echo "==> signing as: $( [ "$IDENTITY" = "-" ] && echo "ad-hoc (no Developer ID certificate)" || echo "$IDENTITY")"
"${SIGN[@]}" -i "$BUNDLE_ID.daemon" "$APP/Contents/MacOS/midnad"
"${SIGN[@]}" -i "$BUNDLE_ID.cli" "$APP/Contents/MacOS/midna"
"${SIGN[@]}" "$APP"
/usr/bin/codesign --verify --deep --strict "$APP"
echo "==> built $APP ($VERSION, $(du -sh "$APP" | cut -f1))"
/usr/sbin/spctl -a -t exec -vv "$APP" 2>&1 | sed 's/^/    gatekeeper: /' || true
