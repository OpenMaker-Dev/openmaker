#!/usr/bin/env bash
# Runs inside the container tools/package-ubuntu.sh starts (see Dockerfile):
# builds release om-gui (with web viewers) and om-server, lays out their
# files, declares the libraries they need, and writes the .deb and the
# tarball to /out. Expects CARGO_HOME, RUSTUP_HOME, CARGO_TARGET_DIR, JOBS
# and VERSION in the environment.
set -euo pipefail
export HOME=/tmp
if [ ! -x "$CARGO_HOME/bin/rustup" ]; then
    curl -sSf https://sh.rustup.rs |
        sh -s -- -y -q --no-modify-path --profile minimal --default-toolchain none
fi
export PATH=$CARGO_HOME/bin:$PATH
cargo build --release --locked -j "$JOBS" -p om-server -p om-gui --features om-gui/web-viewer

# The installed files, under usr/.
bin=$CARGO_TARGET_DIR/release
work=$(mktemp -d)
deb=$work/debian/openmaker  # where dpkg-shlibdeps expects it
install -D -m 755 -s "$bin/om-gui" "$deb/usr/bin/om-gui"
install -m 755 -s "$bin/om-server" "$deb/usr/bin/om-server"
# Each reads PDF data in containers with its worker, beside it (#566).
install -m 755 -s "$bin/om-gui-pdf-worker" "$deb/usr/bin/om-gui-pdf-worker"
install -m 755 -s "$bin/om-server-pdf-worker" "$deb/usr/bin/om-server-pdf-worker"
install -D -m 644 tools/package/openmaker.desktop "$deb/usr/share/applications/openmaker.desktop"
install -D -m 644 favicon.svg "$deb/usr/share/icons/hicolor/scalable/apps/openmaker.svg"
install -D -m 644 favicon.png "$deb/usr/share/icons/hicolor/256x256/apps/openmaker.png"
# The licenses of what the binaries embed: OpenMaker's own, the fonts in
# om-gui/assets, egui's built-in fonts, and the Unicode collation data.
doc=$deb/usr/share/doc/openmaker
egui_fonts=$(grep -A1 '^name = "epaint_default_fonts"' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')
egui_fonts=$(echo "$CARGO_HOME"/registry/src/*/"epaint_default_fonts-$egui_fonts"/fonts)
install -D -m 644 -t "$doc" crates/om-gui/assets/fonts/*LICENSE* \
    crates/om-collate/data/LICENSE-UNICODE
install -D -m 644 -t "$doc/egui-fonts" "$egui_fonts"/*.txt
{
    cat LICENSE
    echo
    echo "om-gui embeds fonts under their own licenses: Inter and Liberation Sans"
    echo "(*-LICENSE.txt here) and egui's built-in fonts, Hack, Ubuntu Light, Noto"
    echo "Emoji, emoji-icon-font and egui-icons (egui-fonts/). om-gui and om-server"
    echo "embed Unicode collation data under the Unicode License (LICENSE-UNICODE)."
} >"$doc/copyright"

# Libraries the binaries link, from their ELF headers (DEBIAN/ marks the
# package root dpkg-shlibdeps looks for).
mkdir -p "$deb/DEBIAN"
printf 'Source: openmaker\n\nPackage: openmaker\nArchitecture: any\n' >"$work/debian/control"
linked=$(cd "$work" && dpkg-shlibdeps -O debian/openmaker/usr/bin/om-gui debian/openmaker/usr/bin/om-server \
    debian/openmaker/usr/bin/om-gui-pdf-worker debian/openmaker/usr/bin/om-server-pdf-worker |
    sed -n 's/^shlibs:Depends=//p')

# Libraries om-gui opens at run time, which the ELF headers do not name (it
# stops without libxkbcommon-x11; its renderer opens Vulkan): run it under
# Xvfb for 15 s and keep what the loader reports opening. What om-gui opens
# itself is required; what those libraries open in turn (Mesa's drivers) is
# recommended, since a desktop with other drivers does not need them.
# Measured under X11 only.
gui=$deb/usr/bin/om-gui
status=0
xvfb-run -a timeout 15 env LD_DEBUG=files LD_DEBUG_OUTPUT="$work/ld" "$gui" >"$work/run.log" 2>&1 ||
    status=$?
if [ "$status" != 124 ]; then
    echo "package: om-gui stopped (exit $status) before 15 s under Xvfb:" >&2
    tail -5 "$work/run.log" >&2
    exit 1
fi
package_of() { # a library's name or path -> its package; names not found go to $work/missing
    local path=$1
    [[ $path == /* ]] || path=$(ldconfig -p | awk -v n="$1" '$1 == n { print $NF; exit }')
    if [ -n "$path" ]; then
        dpkg -S "$(realpath "$path")" | cut -d: -f1
    elif [[ $1 != *.so ]]; then # an unversioned name is a fallback only -dev packages ship
        echo "$1" >>"$work/missing"
    fi
}
opened() { # with ==, the packages of what om-gui opened; with !=, of what the rest opened
    sed -n 's/.*file=\([^ ]*\) \[[0-9]*\];  dynamically loaded by \(.*\) \[[0-9]*\]$/\2\t\1/p' \
        "$work"/ld.* | awk -F'\t' -v gui="$gui" "\$1 $1 gui { print \$2 }" | sort -u |
        while read -r lib; do package_of "$lib"; done | sort -u
}
items() { tr ',' '\n' | sed 's/^ *//' | grep .; }
names() { items | sed 's/ *(.*//' | sort -u; }
required=$(comm -23 <(opened '==') <(echo "$linked" | names))
if [ -s "$work/missing" ]; then
    echo "package: om-gui looked for libraries this image lacks; add their packages to the Dockerfile:" >&2
    sort -u "$work/missing" >&2
    exit 1
fi
recommended=$(comm -23 <(opened '!=') <( (echo "$linked" | names; echo "$required") | sort -u))
list() { paste -sd, - | sed 's/,/, /g'; }
depends=$({ echo "$linked" | items; echo "$required"; } | list)
recommends=$(echo "$recommended" | grep . | list || true)

cat >"$deb/DEBIAN/control" <<CONTROL
Package: openmaker
Version: $VERSION
Architecture: amd64
Maintainer: OpenMaker developers <openmaker@invalid>
Installed-Size: $(du -sk "$deb/usr" | cut -f1)
Section: misc
Priority: optional
Depends: $depends
Recommends: $recommends
Description: open-source client and server for FileMaker Pro files
 om-gui opens and edits .fmp12 files and files hosted by FileMaker Server;
 om-server hosts .fmp12 files for FileMaker Pro clients. Pre-alpha.
CONTROL
dpkg-deb --build --root-owner-group "$deb" "/out/openmaker_${VERSION}_amd64.deb"

# The tarball: the same files under one folder, with the packages to
# install for them listed in share/doc/openmaker/apt-packages.
tarname=openmaker-$VERSION-ubuntu24.04-amd64
mkdir "$work/$tarname"
cp -r "$deb/usr/bin" "$deb/usr/share" "$work/$tarname/"
printf '%s\n' "$depends" "$recommends" | names | grep . >"$work/$tarname/share/doc/openmaker/apt-packages"
tar -C "$work" --owner=0 --group=0 -czf "/out/$tarname.tar.gz" "$tarname"
echo "Depends: $depends"
echo "Recommends: $recommends"
