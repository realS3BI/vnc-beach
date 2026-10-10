#!/bin/zsh
# Builds the iPad/iPhone app for distribution and uploads it to TestFlight.
#
#   tools/release-ios.sh            archive and upload
#   tools/release-ios.sh --no-upload   archive and export only (for testing)
#
# Signing (Apple Distribution, cloud-managed) uses the Apple ID signed in to
# Xcode: an API key needs the Admin role for cloud signing. The upload uses an
# App Store Connect API key, described in ~/.config/vnc-beach/appstoreconnect.env
# (outside the repository):
#
#   ASC_KEY_ID=ABC123DEFG
#   ASC_ISSUER_ID=00000000-0000-0000-0000-000000000000
#   ASC_KEY_PATH=$HOME/.config/vnc-beach/AuthKey_ABC123DEFG.p8
#
# The marketing version comes from apps/ios/Config/Base.xcconfig (release.sh
# keeps it equal to the release); the build number is a timestamp, so every
# upload is unique.
set -euo pipefail
cd "$(dirname "$0")/.."

UPLOAD=1
[ "${1:-}" = "--no-upload" ] && UPLOAD=0

ENV_FILE=${ASC_ENV_FILE:-$HOME/.config/vnc-beach/appstoreconnect.env}
if [ $UPLOAD = 1 ]; then
  [ -f "$ENV_FILE" ] || { print -u2 "release-ios: $ENV_FILE is missing (see the comment at the top of this script)"; exit 1; }
  source "$ENV_FILE"
  [ -f "$ASC_KEY_PATH" ] || { print -u2 "release-ios: API key $ASC_KEY_PATH is missing"; exit 1; }
fi

TEAM=$(sed -n 's/^DEVELOPMENT_TEAM = //p' apps/ios/Config/Signing.local.xcconfig 2>/dev/null || true)
[ -n "$TEAM" ] || { print -u2 "release-ios: DEVELOPMENT_TEAM missing in apps/ios/Config/Signing.local.xcconfig"; exit 1; }
VERSION=$(sed -n 's/^MARKETING_VERSION = //p' apps/ios/Config/Base.xcconfig)
BUILD=$(date -u +%Y%m%d%H%M)
OUT=target/ios
ARCHIVE="$OUT/VNCBeach-$VERSION-$BUILD.xcarchive"
mkdir -p "$OUT"

echo "== archive VNC Beach $VERSION ($BUILD)"
xcodebuild -project apps/ios/VNCBeach.xcodeproj -scheme VNCBeach -configuration Release \
  -destination 'generic/platform=iOS' -archivePath "$ARCHIVE" \
  -allowProvisioningUpdates CURRENT_PROJECT_VERSION="$BUILD" archive \
  | grep -E '^(error|\*\* )|: error:' || true
[ -d "$ARCHIVE" ] || { print -u2 "release-ios: archive failed"; exit 1; }

OPTIONS="$OUT/ExportOptions.plist"
cat > "$OPTIONS" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>method</key><string>app-store-connect</string>
    <key>destination</key><string>export</string>
    <key>teamID</key><string>$TEAM</string>
    <key>signingStyle</key><string>automatic</string>
    <key>manageAppVersionAndBuildNumber</key><false/>
    <key>uploadSymbols</key><true/>
    <key>testFlightInternalTestingOnly</key><true/>
</dict>
</plist>
PLIST

echo "== export"
rm -rf "$OUT/export"
xcodebuild -exportArchive -archivePath "$ARCHIVE" -exportOptionsPlist "$OPTIONS" \
  -exportPath "$OUT/export" -allowProvisioningUpdates \
  | grep -E '^(error|\*\* )|: error:' || true
IPA=$(ls "$OUT/export"/*.ipa 2>/dev/null | head -1)
[ -n "$IPA" ] || { print -u2 "release-ios: export failed"; exit 1; }
echo "== exported: $IPA"
[ $UPLOAD = 1 ] || exit 0

echo "== upload to App Store Connect"
# altool looks for AuthKey_<id>.p8 in API_PRIVATE_KEYS_DIR.
API_PRIVATE_KEYS_DIR=$(dirname "$ASC_KEY_PATH") xcrun altool --upload-package "$IPA" \
  --api-key "$ASC_KEY_ID" --api-issuer "$ASC_ISSUER_ID"
echo "== uploaded; TestFlight shows the build after Apple has processed it (usually 5–30 minutes)"
