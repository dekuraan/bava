#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Build bava-x86_64.AppImage from an already-built binary.
#
#   cargo build --release -p bava --no-default-features
#   packaging/appimage/build-appimage.sh target/release/bava [version]
#
# Feed it the **--no-default-features** (PulseAudio-only) binary: the default
# build links libpipewire-0.3.so, and bundling that into an AppImage is a bad
# idea (the client library talks to whatever pipewire daemon the host runs, and
# version-skewed client/daemon pairs misbehave). The PulseAudio path works on
# both pure-PulseAudio hosts and PipeWire hosts through pipewire-pulse, which is
# exactly the portability an AppImage is for.
set -euo pipefail

bin=${1:?usage: build-appimage.sh <path-to-bava-binary> [version]}
version=${2:-$(git describe --tags --always --dirty 2>/dev/null || echo dev)}

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=${APPIMAGE_WORKDIR:-$root/target/appimage}
appdir=$work/AppDir
tools=${APPIMAGE_TOOLS:-$work/tools}

rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" "$tools"

install -Dm755 "$bin" "$appdir/usr/bin/bava"
install -Dm644 "$root/flatpak/io.github.dekuraan.bava.desktop" \
    "$appdir/usr/share/applications/io.github.dekuraan.bava.desktop"
install -Dm644 "$root/flatpak/io.github.dekuraan.bava.svg" \
    "$appdir/usr/share/icons/hicolor/scalable/apps/io.github.dekuraan.bava.svg"
install -Dm644 "$root/flatpak/io.github.dekuraan.bava.metainfo.xml" \
    "$appdir/usr/share/metainfo/io.github.dekuraan.bava.metainfo.xml"

fetch() {  # url -> $tools/<name>, executable
    local url=$1 dest=$tools/$2
    if [[ ! -x $dest ]]; then
        echo "==> fetching $2" >&2   # stdout is the return channel
        curl -fsSL -o "$dest" "$url"
        chmod +x "$dest"
    fi
    printf '%s' "$dest"
}

base=https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous
ld=$(fetch "$base/linuxdeploy-x86_64.AppImage" linuxdeploy-x86_64.AppImage)
# `--output appimage` is itself a plugin: linuxdeploy resolves it by looking for
# a `linuxdeploy-plugin-appimage` binary on PATH or beside itself, and fails
# with a bare "no such output plugin" if it isn't there.
fetch "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-x86_64.AppImage" \
    linuxdeploy-plugin-appimage-x86_64.AppImage >/dev/null
export PATH="$tools:$PATH"

# --appimage-extract-and-run: CI containers have no FUSE, and an AppImage tool
# that can't mount itself fails with a very unhelpful error.
export APPIMAGE_EXTRACT_AND_RUN=1
export OUTPUT="$work/bava-$version-x86_64.AppImage"
export VERSION="$version"
# linuxdeploy walks the binary's DT_NEEDED chain and copies in everything the
# AppImage may carry, minus its excludelist (glibc, libGL, libvulkan, X11, the
# Wayland client libs …) — the host has to own those or its drivers break.
"$ld" \
    --appdir "$appdir" \
    --executable "$appdir/usr/bin/bava" \
    --desktop-file "$appdir/usr/share/applications/io.github.dekuraan.bava.desktop" \
    --icon-file "$appdir/usr/share/icons/hicolor/scalable/apps/io.github.dekuraan.bava.svg" \
    --output appimage

echo "==> $OUTPUT"
