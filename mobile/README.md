# Building OpenMaker Go

What OpenMaker Go does and its limits are in the
[mobile client guide](../docs/mobile-client.md). The engine (`core/`,
package `openmaker-go`) is a member of the OpenMaker workspace and uses
its crates; `build-core.sh` builds it for an app, optimized, with the
Swift or Kotlin bindings UniFFI generates (`bindings/`, a workspace of its
own). Run the commands below from the repository root. Build output goes
to `target/` and `mobile/build/`.

## iPhone and iPad

On an Apple silicon Mac with Xcode and an iOS SDK:

```sh
rustup target add aarch64-apple-ios-sim
mobile/build-core.sh simulator
xcodebuild -project mobile/apple/OpenMakerGo.xcodeproj -scheme OpenMakerGo \
  -destination 'generic/platform=iOS Simulator' -derivedDataPath mobile/build/xcode build
```

For a device, add `aarch64-apple-ios`, run `mobile/build-core.sh iphoneos`,
open `mobile/apple/OpenMakerGo.xcodeproj` in Xcode, choose a signing team
and build for the device.

## Android

On macOS or Linux with JDK 21, the Android 36 SDK and NDK 27.1.12297006
(`ANDROID_HOME`, default `~/Library/Android/sdk` on macOS and
`~/Android/sdk` on Linux):

```sh
rustup target add aarch64-linux-android
mobile/build-core.sh android
(cd mobile/android && ./gradlew assembleDebug)
```

The APK is in `mobile/android/build/outputs/apk/debug/`. It runs on ARM64
Android 7.0 or newer.

Build the engine again after changing any Rust code, the desktop crates'
included. `GO_PROFILE=dev mobile/build-core.sh ...` builds an unoptimized
engine for a debugger; it is many times slower
([mobile-frames.md](../docs/performance/mobile-frames.md)).

## Checks

```sh
cargo test --locked -p openmaker-go
cargo run --locked -p openmaker-go --example make_example -- "Field Notes.fmp12"
```

The test opens a generated file, taps a button the file defines, types
into a field, closes the file without saving it first (the close saves
it), then hosts the file on a loopback OpenMaker Server and finds the
saved value over TLS. `make_example` writes that file for trying the apps:
import it under My Apps and sign in as Admin with an empty password. Its
Tasks layout has a Name field and a Next task button.

The workspace's checks build the engine for the machine they run on. The
iOS and Android builds also compile `om-gui` without its desktop event
loop (`eframe`, `winit`): after changing what `om-gui` builds on those
targets, run `mobile/build-core.sh` for one of them.

Debug builds of the apps log frames that took more than 16 ms:
`log stream --predicate 'subsystem == "dev.openmaker.go"'` (iOS) and
`adb logcat -s OpenMakerGo` (Android, where
`adb shell setprop log.tag.OpenMakerGo DEBUG` logs every frame with its
engine and paint times).
