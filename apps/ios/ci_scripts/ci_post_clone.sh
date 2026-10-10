#!/bin/sh
# Xcode Cloud: install XcodeGen and a Rust toolchain with the iOS target,
# write the team and build number, regenerate the project.
set -eu

ios_directory=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if [ "${CI_XCODE_CLOUD:-}" != "TRUE" ]; then
  echo "This script only runs inside Xcode Cloud." >&2
  exit 1
fi
case "${VNC_BEACH_DEVELOPMENT_TEAM:-}" in
  ''|*[!A-Z0-9]*) echo "VNC_BEACH_DEVELOPMENT_TEAM is missing or invalid." >&2; exit 1 ;;
esac
if [ "${#VNC_BEACH_DEVELOPMENT_TEAM}" -ne 10 ]; then
  echo "VNC_BEACH_DEVELOPMENT_TEAM must contain 10 characters." >&2
  exit 1
fi
case "${CI_BUILD_NUMBER:-}" in
  ''|*[!0-9]*|0) echo "CI_BUILD_NUMBER must be a positive integer." >&2; exit 1 ;;
esac
if [ -e "$ios_directory/Config/Signing.local.xcconfig" ]; then
  echo "Local signing configuration must not be present in the cloud checkout." >&2
  exit 1
fi

umask 077
printf 'DEVELOPMENT_TEAM = %s\nCURRENT_PROJECT_VERSION = %s\n' \
  "$VNC_BEACH_DEVELOPMENT_TEAM" "$CI_BUILD_NUMBER" > "$ios_directory/Config/Cloud.xcconfig"

export HOMEBREW_NO_AUTO_UPDATE=1
if ! command -v xcodegen >/dev/null 2>&1; then
  brew install xcodegen
fi

# Rust for crates/beach-mobile; tools/build-ios.sh looks in ~/.cargo/bin first.
if [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable \
        --target aarch64-apple-ios,aarch64-apple-ios-sim --no-modify-path
fi

cd "$ios_directory"
xcodegen generate --spec project.yml
