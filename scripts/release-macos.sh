#!/bin/sh
# Builds the macOS release: a Developer ID signed, notarized app, packaged as
# dist/KoeBoard-macos-<arch>.dmg (for people) and .zip (for the in-app updater).
#
# One-time setup, which stores an app-specific password in the keychain:
#   xcrun notarytool store-credentials koeboard --apple-id <id> --team-id <team>
set -eu
cd "$(dirname "$0")/.."

CODESIGN_IDENTITY=${CODESIGN_IDENTITY:-Developer ID Application}
export CODESIGN_IDENTITY
profile=${NOTARY_PROFILE:-koeboard}

notarize() {
  xcrun notarytool submit "$1" --keychain-profile "$profile" --wait
  xcrun stapler staple "$2"
}

./scripts/bundle-macos.sh

app=dist/KoeBoard.app
case "$(uname -m)" in arm64) arch=arm64 ;; *) arch=x64 ;; esac
zip="dist/KoeBoard-macos-$arch.zip"
dmg="dist/KoeBoard-macos-$arch.dmg"

# The ticket must be stapled before packaging so the app also passes Gatekeeper offline.
notarize "$zip" "$app"
spctl --assess --type execute --verbose=2 "$app"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
ditto "$app" "$stage/KoeBoard.app"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname KoeBoard -srcfolder "$stage" -format UDZO "$dmg"
codesign --force --timestamp --sign "$CODESIGN_IDENTITY" "$dmg"
notarize "$dmg" "$dmg"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"

echo "$dmg"
echo "$zip"
