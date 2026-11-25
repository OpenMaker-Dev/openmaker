#!/usr/bin/env bash
# Builds the desktop client for macOS and puts it in an app bundle,
# OpenMaker.app: release om-gui (with web viewers) and its PDF worker, an
# Info.plist and the icon. Run from a bundle, the Dock, Command-Tab and
# Finder name the app OpenMaker; run from the build, LaunchServices names
# it after its file, om-gui (#821). See docs/development.md.
#
#   tools/package-macos.sh [output folder]     (default: target/package)
#
# Needs stable Rust and the Xcode command line tools; everything else ships
# with macOS. It builds for this Mac's architecture and signs ad hoc, so
# the bundle is for this Mac. JOBS is cargo's job count (default: cargo's
# own).
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Darwin ]; then
    echo "package-macos: run it on macOS" >&2
    exit 1
fi
out=${1:-target/package}
# The programs where cargo says it put them: its environment and
# configuration can move them (CARGO_TARGET_DIR, build.target-dir,
# build.target). Its messages go to stdout, its diagnostics to stderr.
built=$(cargo build --release --locked ${JOBS:+-j "$JOBS"} -p om-gui --features web-viewer --bins \
    --message-format=json-render-diagnostics)
executable() { # the path cargo gave for the program named $1
    printf '%s\n' "$built" | { grep '"executable":"' || true; } | while IFS= read -r line; do
        path=$(printf '%s' "$line" | plutil -extract executable raw -o - -)
        if [ "$(basename "$path")" = "$1" ]; then
            echo "$path"
        fi
    done
}
gui=$(executable om-gui)
worker=$(executable om-gui-pdf-worker)
if [ ! -f "$gui" ] || [ ! -f "$worker" ]; then
    echo "package-macos: cargo reported no om-gui or om-gui-pdf-worker program" >&2
    exit 1
fi

mkdir -p "$out"
# Staged beside the bundle it replaces, so the last step is a rename.
work=$(mktemp -d "$out/.OpenMaker.XXXXXX")
# The lock is a link to this run's process id, made at once or not at
# all, so cleanup knows whether it is this run's.
lock=$out/.OpenMaker.lock
cleanup() {
    set +e
    # A run stopped between the two renames below puts the previous
    # bundle back.
    if [ -e "$work/previous.app" ] && [ ! -e "$out/OpenMaker.app" ]; then
        mv "$work/previous.app" "$out/OpenMaker.app"
    fi
    rm -rf "$work"
    if [ "$(readlink "$lock")" = "$$" ]; then
        rm -f "$lock"
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
app=$work/OpenMaker.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
# om-gui looks for its PDF worker beside itself (#566).
cp "$gui" "$worker" "$app/Contents/MacOS/"

# The Info.plist build.rs embeds in om-gui (name, identifier, version),
# plus what a bundle adds: its program, its type and its icon.
plist=$app/Contents/Info.plist
segedit "$gui" -extract __TEXT __info_plist "$plist"
plutil -insert CFBundleExecutable -string om-gui "$plist"
plutil -insert CFBundlePackageType -string APPL "$plist"
plutil -insert CFBundleIconFile -string OpenMaker "$plist"
plutil -insert NSHighResolutionCapable -bool YES "$plist"
plutil -convert xml1 "$plist"
plutil -lint -s "$plist"

# The icon: the app icon om-gui sets at run time (512 x 512, the macOS
# icon grid; docs/assets/README.md) at the sizes Finder and the Dock use.
# There is no 1024-pixel source, so the iconset has no 512@2x.
iconset=$work/OpenMaker.iconset
mkdir "$iconset"
source_icon=crates/om-gui/assets/openmaker-icon.png
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$source_icon" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    if [ "$size" -lt 512 ]; then
        double=$((size * 2))
        sips -z "$double" "$double" "$source_icon" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    fi
done
iconutil -c icns -o "$app/Contents/Resources/OpenMaker.icns" "$iconset"
rm -r "$iconset"

# Ad-hoc signatures, the worker first: the bundle's seals its Info.plist
# and icon, which the linker's signature of om-gui alone does not.
codesign --force --sign - "$app/Contents/MacOS/om-gui-pdf-worker"
codesign --force --sign - "$app"
codesign --verify --strict "$app"

# A link there would be moved, not what it points to.
if [ -L "$out/OpenMaker.app" ]; then
    echo "package-macos: $out/OpenMaker.app is a symbolic link; remove it or give another folder" >&2
    exit 1
fi
# One run at a time replaces the bundle: mv would move a second bundle
# into one another run had just put in place.
if [ -d "$lock" ] && [ ! -L "$lock" ] || ! ln -s "$$" "$lock" 2>/dev/null; then
    echo "package-macos: another run is replacing $out/OpenMaker.app; if none is, remove $lock" >&2
    exit 1
fi
if [ -e "$out/OpenMaker.app" ]; then
    mv "$out/OpenMaker.app" "$work/previous.app"
fi
mv "$app" "$out/OpenMaker.app"
echo "$out/OpenMaker.app"
