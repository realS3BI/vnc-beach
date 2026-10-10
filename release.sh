#!/bin/zsh
# Releases VNC Beach on all platforms:
#
#   1. sets the version in Cargo.toml and the iOS project,
#   2. runs the tests, builds the macOS app here and the Windows exe on the PC,
#   3. commits "Release vX.Y.Z", tags it and pushes main and the tag,
#   4. creates the GitHub release with VNC-Beach-macOS.zip and
#      VNC-Beach-Windows.exe (the apps' updaters download these),
#   5. builds the iPad/iPhone app and uploads it to TestFlight
#      (tools/release-ios.sh, App Store Connect API key in
#      ~/.config/vnc-beach/appstoreconnect.env).
#
# Usage:
#   ./release.sh 0.2.0                 full release
#   ./release.sh 0.2.0 --dry-run       build everything, publish nothing,
#                                      restore the version files afterwards
#   Options: --skip-macos --skip-windows --skip-ios
#
# Needs: a clean main that is not behind origin, `gh` logged in, the PC
# reachable as `sebastian-pc` (for Windows), the App Store Connect key (for
# iOS) and the toolchains described in AGENTS.md. The first release may use
# the current version (0.1.0); later ones need a higher one.
set -euo pipefail
cd "$(dirname "$0")"

die() { print -u2 "release: $*"; exit 1; }
step() { print "\n== $*"; }

[ $# -ge 1 ] || die "usage: ./release.sh X.Y.Z [--dry-run] [--skip-macos] [--skip-windows] [--skip-ios]"
VERSION=$1
shift
DRY=0 SKIP_MAC=0 SKIP_WIN=0 SKIP_IOS=0
for arg in "$@"; do
  case $arg in
    --dry-run) DRY=1 ;;
    --skip-macos) SKIP_MAC=1 ;;
    --skip-windows) SKIP_WIN=1 ;;
    --skip-ios) SKIP_IOS=1 ;;
    *) die "unknown option $arg" ;;
  esac
done

[[ $VERSION =~ '^[0-9]+\.[0-9]+\.[0-9]+$' ]] || die "version must look like 1.2.3, got '$VERSION'"
TAG="v$VERSION"
CURRENT=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
[ -n "$CURRENT" ] || die "no version in [workspace.package] of Cargo.toml"

# Numeric comparison of X.Y.Z. The current version itself is fine as long as
# it hasn't been released (checked with the tag below).
newer=$(python3 -c "import sys; a,b=[tuple(map(int,v.split('.'))) for v in sys.argv[1:]]; print(int(a>b), int(a==b))" "$VERSION" "$CURRENT")
if [ "$newer" = "0 0" ]; then
  die "$VERSION is lower than the current version $CURRENT"
fi

step "VNC Beach $CURRENT -> $VERSION$([ $DRY = 1 ] && print ' (dry run)')"

if [ $DRY = 0 ]; then
  [ "$(git branch --show-current)" = main ] || die "releases are made from main"
  [ -z "$(git status --porcelain)" ] || die "the working tree is not clean; commit or stash first"
  git fetch --quiet origin
  git merge-base --is-ancestor origin/main HEAD || die "main is behind origin/main; pull first"
  git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && die "tag $TAG exists already"
  git ls-remote --exit-code --tags origin "$TAG" >/dev/null 2>&1 && die "tag $TAG exists on GitHub already"
  gh auth status >/dev/null 2>&1 || die "gh is not logged in (gh auth login)"
fi
if [ $SKIP_IOS = 0 ] && [ $DRY = 0 ] && [ ! -f "$HOME/.config/vnc-beach/appstoreconnect.env" ]; then
  die "no App Store Connect key (~/.config/vnc-beach/appstoreconnect.env); set it up or use --skip-ios"
fi
if [ $SKIP_WIN = 0 ]; then
  ssh -o ConnectTimeout=8 -o BatchMode=yes sebastian-pc 'exit' 2>/dev/null \
    || die "the PC (sebastian-pc) is not reachable; start it or use --skip-windows"
fi

VERSION_FILES=(Cargo.toml Cargo.lock crates/*/Cargo.toml apps/ios/Config/Base.xcconfig)
if [ $DRY = 1 ]; then
  BACKUP=$(mktemp -d)
  for f in $VERSION_FILES; do mkdir -p "$BACKUP/${f:h}"; cp "$f" "$BACKUP/$f"; done
  restore() {
    for f in $VERSION_FILES; do cp "$BACKUP/$f" "$f"; done
    rm -rf "$BACKUP"
    print "\n(dry run: version files restored)"
  }
  trap restore EXIT
fi

step "set version $VERSION"
python3 - "$CURRENT" "$VERSION" <<'PY'
import glob, re, sys
old, new = sys.argv[1], sys.argv[2]

def sub(path, pattern, repl, count=0):
    text = open(path).read()
    changed, n = re.subn(pattern, repl, text, count=count, flags=re.M)
    if n == 0:
        sys.exit(f"release: nothing to change in {path}")
    open(path, "w").write(changed)

sub("Cargo.toml", r'(\[workspace\.package\]\n(?:.*\n)*?version = ")[^"]*(")', rf"\g<1>{new}\g<2>", 1)
for manifest in glob.glob("crates/*/Cargo.toml"):
    text = open(manifest).read()
    text = re.sub(rf'(version = "){re.escape(old)}(", path = ")', rf"\g<1>{new}\g<2>", text)
    open(manifest, "w").write(text)
sub("apps/ios/Config/Base.xcconfig", r"^MARKETING_VERSION = .*$", f"MARKETING_VERSION = {new}")
PY
cargo update --workspace --quiet

step "test"
if ! out=$(cargo test --workspace --quiet 2>&1); then
  print -r -- "$out" | tail -30
  die "tests failed"
fi
print "$(print -r -- "$out" | grep -c '^test result: ok') test suites passed"

ASSETS=()
if [ $SKIP_MAC = 0 ]; then
  step "macOS"
  ./tools/build-macos-app.sh 2>&1 | grep -E '^==|^error|Architectures'
  ASSETS+=(target/macos/VNC-Beach-macOS.zip)
fi
if [ $SKIP_WIN = 0 ]; then
  step "Windows"
  ./tools/build-windows.sh 2>&1 | grep -E '^==|^error|expected'
  ASSETS+=(target/windows/VNC-Beach-Windows.exe)
fi

if [ $DRY = 1 ]; then
  step "dry run finished, nothing published"
  print "Would commit \"Release $TAG\", tag $TAG, push main and the tag,"
  print "create the GitHub release with: ${ASSETS[*]:-no assets}"
  [ $SKIP_IOS = 0 ] && print "and upload the iOS app to TestFlight."
  exit 0
fi

step "commit and tag"
git add -- $VERSION_FILES
# Nothing to commit when releasing the version that is already set.
git diff --cached --quiet || git commit --quiet -m "Release $TAG"
git tag -a "$TAG" -m "VNC Beach $VERSION"
git push --quiet origin main "$TAG"

step "GitHub release"
gh release create "$TAG" $ASSETS --verify-tag --title "VNC Beach $VERSION" --generate-notes \
  || die "creating the release failed; the tag is pushed, retry with: gh release create $TAG ${ASSETS[*]} --verify-tag --title 'VNC Beach $VERSION' --generate-notes"

if [ $SKIP_IOS = 0 ]; then
  step "iOS -> TestFlight"
  ./tools/release-ios.sh || die "the iOS upload failed; GitHub release is done, retry with: tools/release-ios.sh"
fi

step "done"
gh release view "$TAG" --json url --jq .url
