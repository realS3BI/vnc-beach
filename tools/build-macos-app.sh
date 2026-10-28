#!/bin/zsh
# Builds "VNC Beach.app" for macOS: a universal (arm64 + x86_64) viewer with
# the built-in server, signed and zipped to target/macos/.
#
# Signing uses the first "Developer ID Application" or "Apple Development"
# identity in the keychain (override with SIGN_IDENTITY). A stable identity
# matters: macOS ties the Screen Recording and Accessibility permissions to
# it, so an ad-hoc signed update would have to be allowed again.
#
# Needs: the default toolchain for arm64, and for x86_64 the Homebrew rustup
# toolchain with `rustup target add x86_64-apple-darwin` (skipped if absent).
#
# BEACH_CAMERA=1 also builds the camera extension (apps/macos-camera) with
# Xcode's automatic signing and embeds it with the host's provisioning profile.
# With an "Apple Development" profile the app then only starts on Macs
# registered in that profile, so release builds leave it out for now.
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')
OUT=target/macos
APP="$OUT/VNC Beach.app"
RUSTUP=/opt/homebrew/opt/rustup/bin/rustup
CAMERA=${BEACH_CAMERA:-0}

echo "== arm64"
cargo build --release -p beach-viewer
ARM=target/release/beach-viewer
BIN="$OUT/vnc-beach"
mkdir -p "$OUT"
if [ -x "$RUSTUP" ] && "$RUSTUP" target list --installed 2>/dev/null | grep -q x86_64-apple-darwin; then
  echo "== x86_64"
  # The rustup proxies must shadow Homebrew's rustc, or cargo picks the wrong compiler.
  PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo build --release -p beach-viewer --target x86_64-apple-darwin
  lipo -create "$ARM" target/x86_64-apple-darwin/release/beach-viewer -output "$BIN"
else
  echo "== x86_64 target not installed, arm64 only"
  cp "$ARM" "$BIN"
fi
lipo -info "$BIN"

echo "== bundle"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/vnc-beach"
cp assets/icon.icns "$APP/Contents/Resources/icon.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>de</string>
    <key>CFBundleDisplayName</key><string>VNC Beach</string>
    <key>CFBundleExecutable</key><string>vnc-beach</string>
    <key>CFBundleIconFile</key><string>icon</string>
    <key>CFBundleIdentifier</key><string>at.schlossers.vnc-beach</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>VNC Beach</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>MIT License</string>
    <key>NSCameraUsageDescription</key><string>VNC Beach gibt die Kamera eines verbundenen Viewers an die Kamera „VNC Beach“ weiter.</string>
</dict>
</plist>
PLIST
ENTITLEMENTS=""
if [ "$CAMERA" = 1 ]; then
  echo "== camera extension"
  DD=target/macos/camera-build
  command -v xcodegen >/dev/null && (cd apps/macos-camera && xcodegen generate --quiet)
  xcodebuild -project apps/macos-camera/VNCBeachCamera.xcodeproj -scheme Host -configuration Release \
    -derivedDataPath "$DD" -allowProvisioningUpdates build \
    | grep -E '^(error|\*\* )|: error:' || true
  HOST="$DD/Build/Products/Release/VNC Beach.app"
  [ -d "$HOST/Contents/Library/SystemExtensions" ] || { print -u2 "camera extension build failed"; exit 1; }
  mkdir -p "$APP/Contents/Library"
  ditto "$HOST/Contents/Library/SystemExtensions" "$APP/Contents/Library/SystemExtensions"
  cp "$HOST/Contents/embedded.provisionprofile" "$APP/Contents/embedded.provisionprofile"
  # The app must be signed with the certificate the profile was made for,
  # which is the one Xcode signed the extension with.
  CERTS=$(mktemp -d)
  EXT=$(print -r -- "$PWD/$HOST"/Contents/Library/SystemExtensions/*.systemextension)
  (cd "$CERTS" && codesign -d --extract-certificates "$EXT" 2>/dev/null)
  SIGN_IDENTITY=$(openssl x509 -inform DER -in "$CERTS/codesign0" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')
  TEAM=$(security cms -D -i "$APP/Contents/embedded.provisionprofile" | plutil -extract TeamIdentifier.0 raw -)
  ENTITLEMENTS="$CERTS/app.entitlements"
  cat > "$ENTITLEMENTS" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.application-identifier</key><string>$TEAM.at.schlossers.vnc-beach</string>
    <key>com.apple.developer.team-identifier</key><string>$TEAM</string>
    <key>com.apple.developer.system-extension.install</key><true/>
    <key>com.apple.security.application-groups</key><array><string>$TEAM.at.schlossers.vnc-beach</string></array>
    <key>com.apple.security.device.camera</key><true/>
</dict>
</plist>
PLIST
fi
# First usable identity, Developer ID before Apple Development. The keychain
# can hold revoked certificates that still count as "valid"; an app signed
# with one never starts, so probe each with a throwaway binary first.
pick_identity() {
  local kind h probe
  probe=$(mktemp -d)/probe
  for kind in "Developer ID Application" "Apple Development"; do
    for h in $(security find-identity -v -p codesigning 2>/dev/null | grep "\"$kind:" | awk '{print $2}'); do
      cp /usr/bin/true "$probe"
      codesign --force --sign "$h" "$probe" 2>/dev/null || continue
      # Capture first: with pipefail, `spctl | grep -q` fails on SIGPIPE.
      local verdict=$(spctl -a -t exec -vv "$probe" 2>&1 || true)
      if [[ $verdict != *REVOKED* ]]; then
        print "$h"
        return
      fi
    done
  done
}
IDENTITY=${SIGN_IDENTITY:-$(pick_identity)}
if [ -n "$IDENTITY" ]; then
  echo "== signing with $(security find-identity -v -p codesigning | grep "$IDENTITY" | sed 's/.*"\(.*\)"/\1/')"
  if [ -n "$ENTITLEMENTS" ]; then
    # Not --deep: the extension keeps Xcode's signature and entitlements.
    codesign --force --options runtime --timestamp=none --entitlements "$ENTITLEMENTS" --sign "$IDENTITY" "$APP"
  else
    codesign --force --deep --options runtime --timestamp=none --sign "$IDENTITY" "$APP"
  fi
else
  echo "== no signing identity, ad-hoc signature (permissions must be granted again after updates)"
  codesign --force --deep --sign - "$APP"
fi
codesign --verify --verbose=1 "$APP"
rm -f "$OUT/VNC-Beach-macOS.zip"
ditto -c -k --keepParent "$APP" "$OUT/VNC-Beach-macOS.zip"
echo "== done: $APP ($(du -sh "$OUT/VNC-Beach-macOS.zip" | cut -f1) zip)"
