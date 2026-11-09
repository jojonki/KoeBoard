#!/bin/sh
# Builds dist/KoeBoard.app. macOS grants microphone access per app bundle.
set -eu
cd "$(dirname "$0")/.."
cargo build --release
app=dist/KoeBoard.app
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp assets/icons/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
cp target/release/koeboard "$app/Contents/MacOS/koeboard"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>KoeBoard</string>
  <key>CFBundleIdentifier</key><string>dev.koeboard.dictation</string>
  <key>CFBundleExecutable</key><string>koeboard</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>KoeBoard uses the microphone for dictation. Audio never leaves this device.</string>
</dict>
</plist>
PLIST
# macOS keys privacy grants to the requirement below. The default ad-hoc requirement
# is the build's hash, which would drop Accessibility access on every rebuild.
# Release builds set CODESIGN_IDENTITY; see scripts/release-macos.sh.
if [ -n "${CODESIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp \
    --entitlements scripts/KoeBoard.entitlements --sign "$CODESIGN_IDENTITY" "$app"
else
  codesign --force --sign - -r='designated => identifier "dev.koeboard.dictation"' "$app"
fi
# The in-app updater looks for exactly this asset name on the release.
case "$(uname -m)" in arm64) arch=arm64 ;; *) arch=x64 ;; esac
zip="dist/KoeBoard-macos-$arch.zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "$app"
echo "$zip"
