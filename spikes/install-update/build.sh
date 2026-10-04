#!/bin/zsh
# Usage: ./build.sh <version> [broken]   -> dist/<version>/{Midna.app, Midna.app.tar.gz, .sig}
set -eu
D=${0:A:h}; cd $D
V=$1; ID="Developer ID Application: Morgan Hunt (U2G2XV3688)"
mkdir -p keys dist
if [[ ! -f keys/updater.key ]]; then
  cargo build --release -q --bin spike-sign
  target/release/spike-sign keygen keys/updater.key > keys/updater.pub
fi
export MIDNA_UPDATER_PUBKEY=$(cat keys/updater.pub) MIDNA_VERSION=$V
if [[ ${2:-} == broken ]]; then export MIDNA_BROKEN=1; else unset MIDNA_BROKEN; fi
cargo build --release -q --bin midna --bin midnad --bin spike-sign
O=dist/$V; rm -rf $O; A=$O/Midna.app; mkdir -p $A/Contents/{MacOS,Library/LaunchAgents}
cp target/release/midna target/release/midnad $A/Contents/MacOS/
cat > $A/Contents/Info.plist <<P
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.mrgnhnt.midna</string>
  <key>CFBundleName</key><string>Midna</string>
  <key>CFBundleExecutable</key><string>midna</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$V</string>
  <key>CFBundleVersion</key><string>$V</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
</dict></plist>
P
cat > $A/Contents/Library/LaunchAgents/com.mrgnhnt.midna.daemon.plist <<P
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.mrgnhnt.midna.daemon</string>
  <key>BundleProgram</key><string>Contents/MacOS/midnad</string>
  <key>ProgramArguments</key><array><string>Contents/MacOS/midnad</string><string>serve</string></array>
  <key>AssociatedBundleIdentifiers</key><array><string>com.mrgnhnt.midna</string></array>
  <key>KeepAlive</key><true/>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict></plist>
P
# inside-out signing: nested executables first, then the bundle
codesign --force --sign "$ID" --options runtime --timestamp -i com.mrgnhnt.midna.daemon $A/Contents/MacOS/midnad
codesign --force --sign "$ID" --options runtime --timestamp $A
(cd $O && COPYFILE_DISABLE=1 tar czf Midna.app.tar.gz Midna.app)
target/release/spike-sign sign keys/updater.key $O/Midna.app.tar.gz > $O/Midna.app.tar.gz.sig
echo built $O
