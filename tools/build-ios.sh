#!/bin/sh
# Builds crates/beach-mobile as a static library for iOS.
#
#   tools/build-ios.sh aarch64-apple-ios        # device
#   tools/build-ios.sh aarch64-apple-ios-sim    # simulator
#
# Xcode runs this from a build phase of apps/ios (see project.yml). It needs a
# rustup-managed toolchain with the target installed; the Homebrew `rust`
# formula has no iOS targets. On this Mac: `brew install rustup`, then
# `rustup toolchain install stable --target aarch64-apple-ios,aarch64-apple-ios-sim`.
set -eu

target=${1:-aarch64-apple-ios}
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# Xcode exports SDK paths for the C compiler that confuse cargo's build scripts.
unset SDKROOT IPHONEOS_DEPLOYMENT_TARGET MACOSX_DEPLOYMENT_TARGET
unset CC CXX LD CFLAGS CXXFLAGS LDFLAGS
export CARGO_TERM_COLOR=never

cargo=""
for candidate in "$HOME/.cargo/bin/cargo" "$(brew --prefix rustup 2>/dev/null || true)/bin/cargo" \
    /opt/homebrew/opt/rustup/bin/cargo "$(command -v cargo || true)"; do
  [ -n "$candidate" ] && [ -x "$candidate" ] || continue
  sysroot=$("$(dirname "$candidate")/rustc" --print sysroot 2>/dev/null || true)
  if [ -n "$sysroot" ] && [ -d "$sysroot/lib/rustlib/$target/lib" ]; then
    cargo=$candidate
    break
  fi
done
if [ -z "$cargo" ]; then
  echo "error: no Rust toolchain with target $target found (install rustup and run: rustup target add $target)" >&2
  exit 1
fi

# cargo looks up rustc on PATH; make sure it finds the matching toolchain.
PATH="$(dirname "$cargo"):$PATH"
export PATH
cd "$root"
# The workspace uses thin LTO, which leaves LLVM bitcode objects in the static
# library; Apple's linker (an older LLVM) can't read them. Fat LTO links
# everything into native objects inside rustc.
export CARGO_PROFILE_RELEASE_LTO=fat
"$cargo" build --release -p beach-mobile --target "$target"
echo "built target/$target/release/libbeach_mobile.a"
