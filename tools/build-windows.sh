#!/bin/zsh
# Builds the Windows release on Sebastian's PC (SSH alias sebastian-pc, GNU
# toolchain, see AGENTS.md) and copies it to target/windows/VNC-Beach-Windows.exe.
set -euo pipefail
cd "$(dirname "$0")/.."
HOST=${WINDOWS_HOST:-sebastian-pc}
OUT=target/windows
mkdir -p "$OUT"

echo "== sync source to $HOST"
COPYFILE_DISABLE=1 tar --exclude ./target --exclude ./.git --exclude ./apps/ios/DerivedData -cf - . \
  | ssh "$HOST" 'tar -xf - -C %LOCALAPPDATA%\vnc-beach\src'
echo "== build on $HOST"
ssh "$HOST" '%LOCALAPPDATA%\vnc-beach\cargo.cmd build --release -p beach-viewer 2>&1' \
  | grep -vE '^\s+(Compiling|Downloading|Downloaded)' || true
echo "== fetch"
rm -f "$OUT/VNC-Beach-Windows.exe"
# ${HOST} in braces: zsh would read "$HOST:A..." as a path modifier.
scp -q "${HOST}:AppData/Local/vnc-beach/src/target/release/beach-viewer.exe" "$OUT/VNC-Beach-Windows.exe"
size=$(stat -f %z "$OUT/VNC-Beach-Windows.exe")
[ "$size" -gt 1000000 ] || { echo "Windows build looks broken ($size bytes)" >&2; exit 1; }
version=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')
embedded=$(ssh "$HOST" 'powershell -NoProfile -Command "(Get-Item $env:LOCALAPPDATA\vnc-beach\src\target\release\beach-viewer.exe).VersionInfo.ProductVersion"' | tr -d '\r')
if [ "$embedded" != "$version" ]; then
  echo "Windows exe reports version '$embedded', expected $version" >&2
  exit 1
fi
echo "== done: $OUT/VNC-Beach-Windows.exe ($(du -h "$OUT/VNC-Beach-Windows.exe" | cut -f1))"
