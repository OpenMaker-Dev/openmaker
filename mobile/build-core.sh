#!/usr/bin/env bash
# Build OpenMaker Go's engine for one app, optimized, with the bindings its
# native code calls, into mobile/build/:
#
#   simulator  iOS and iPadOS Simulator (Apple silicon Mac)
#   iphoneos   iPhone and iPad (Mac)
#   android    ARM64 Android (Mac or Linux with the Android NDK)
#
# A debug engine is many times slower (docs/performance/mobile-frames.md);
# set GO_PROFILE=dev to build one anyway, for a debugger.
set -euo pipefail
platform=${1:-}
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
profile=${GO_PROFILE:-release}
case $profile in
  release) profile_dir=release ;;
  dev) profile_dir=debug ;;
  *) echo "GO_PROFILE is release or dev" >&2; exit 2 ;;
esac
args=()
case $platform in
  simulator | iphoneos)
    [[ $platform == simulator ]] && target=aarch64-apple-ios-sim || target=aarch64-apple-ios
    lang=swift
    # The static library links into the app; bindings are read from the
    # dynamic one.
    types=staticlib,cdylib
    export IPHONEOS_DEPLOYMENT_TARGET=16.0
    ;;
  android)
    target=aarch64-linux-android
    lang=kotlin
    types=cdylib
    case "$(uname)" in
      Darwin) host=darwin-x86_64 sdk_default=$HOME/Library/Android/sdk ;;
      Linux) host=linux-x86_64 sdk_default=$HOME/Android/sdk ;;
      *) echo "Build Android on macOS or Linux" >&2; exit 2 ;;
    esac
    sdk=${ANDROID_HOME:-$sdk_default}
    ndk=${ANDROID_NDK_HOME:-$sdk/ndk/27.1.12297006}
    bin="$ndk/toolchains/llvm/prebuilt/$host/bin"
    [[ -x $bin/clang ]] || { echo "No Android NDK at $ndk" >&2; exit 2; }
    export CC_aarch64_linux_android="$bin/aarch64-linux-android24-clang"
    export CXX_aarch64_linux_android="$bin/aarch64-linux-android24-clang++"
    export AR_aarch64_linux_android="$bin/llvm-ar"
    export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC_aarch64_linux_android"
    # 16 KB pages, which Android 15 devices may use.
    args=(-C link-arg=-Wl,-z,max-page-size=16384)
    ;;
  *)
    echo "usage: $0 simulator|iphoneos|android" >&2
    exit 2
    ;;
esac
cargo rustc --locked --profile "$profile" -p openmaker-go --lib --target "$target" \
  --crate-type "$types" -- "${args[@]}"
target_dir=${CARGO_TARGET_DIR:-$root/target}
built="$target_dir/$target/$profile_dir"
out="mobile/build/$platform"
rm -rf "$out"
mkdir -p "$out"
if [[ $lang == kotlin ]]; then
  library="$built/libopenmaker_go.so"
else
  library="$built/libopenmaker_go.dylib"
fi
cargo run --locked --release --manifest-path mobile/bindings/Cargo.toml -- generate \
  --library "$library" --language "$lang" --config mobile/core/uniffi.toml \
  --out-dir "$out/bindings" --no-format
if [[ $lang == kotlin ]]; then
  mkdir -p "$out/lib/arm64-v8a"
  "$bin/llvm-strip" --strip-unneeded "$library" -o "$out/lib/arm64-v8a/libopenmaker_go.so"
else
  # Only the static library: next to a dylib the linker would take that.
  cp "$built/libopenmaker_go.a" "$out/"
  mkdir -p mobile/build/apple
  cp "$out"/bindings/*.swift "$out"/bindings/*.h "$out"/bindings/*.modulemap mobile/build/apple/
fi
echo "Built $out ($profile)"
