#!/bin/sh
# Builds dist/KoeBoard.app. macOS grants microphone access per app bundle.
set -eu
cd "$(dirname "$0")/.."
cargo build --release
app=dist/KoeBoard.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/koeboard "$app/Contents/MacOS/koeboard"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>KoeBoard</string>
  <key>CFBundleIdentifier</key><string>dev.koeboard.dictation</string>
  <key>CFBundleExecutable</key><string>koeboard</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>KoeBoard uses the microphone for dictation. Audio never leaves this device.</string>
</dict>
</plist>
PLIST
# macOS keys privacy grants to the requirement below. The default ad-hoc requirement
# is the build's hash, which would drop Accessibility access on every rebuild.
codesign --force --sign - -r='designated => identifier "dev.koeboard.dictation"' "$app"
echo "$app"
