#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Regenerate docs/screenshot.png — the shot AppStream/Flathub requires.
#
#   packaging/screenshot.sh [bava-binary]
#
# It renders through bava's own offline path (`--input … --out …`), so the image
# is the real renderer, HUD, and colour pipeline rather than a mockup. The audio
# is synthesised on the spot: a chord, a rising sweep, and pink noise, which
# gives every band something to show. Deterministic — same bytes every run.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bava=${1:-$root/target/release/bava}
work=${SCREENSHOT_WORKDIR:-$root/target/screenshot}
out=$root/docs/screenshot.png

command -v ffmpeg >/dev/null || { echo "ffmpeg is required" >&2; exit 1; }
[[ -x $bava ]] || { echo "no bava binary at $bava (cargo build --release -p bava)" >&2; exit 1; }

mkdir -p "$work" "$(dirname "$out")"
clip=$work/tone.flac

echo "==> synthesising audio"
# A chord in the bass, a rising sweep across the mids, and pink noise on top:
# cavacore's bands are log-spaced, so the spectrum needs energy spread over
# octaves rather than one loud tone. The tremolo rates are all different, so no
# two bars move together.
ffmpeg -y -loglevel error \
    -f lavfi -i "sine=frequency=110:duration=8" \
    -f lavfi -i "sine=frequency=220:duration=8" \
    -f lavfi -i "sine=frequency=330:duration=8" \
    -f lavfi -i "sine=frequency=550:duration=8" \
    -f lavfi -i "aevalsrc='0.6*sin(2*PI*(300+1400*t)*t)':d=8" \
    -f lavfi -i "anoisesrc=color=pink:duration=8:amplitude=0.3" \
    -filter_complex "\
        [0:a]tremolo=f=2:d=0.8[a0]; \
        [1:a]tremolo=f=3:d=0.7[a1]; \
        [2:a]tremolo=f=5:d=0.7[a2]; \
        [3:a]tremolo=f=7:d=0.6[a3]; \
        [a0][a1][a2][a3][4:a][5:a]amix=inputs=6:normalize=0,\
        volume=0.35,aformat=sample_fmts=s16:channel_layouts=stereo[out]" \
    -map "[out]" \
    -metadata title="Spectrum Test Tone" \
    -metadata artist="bava" \
    -metadata album="Synthetic" \
    "$clip"

echo "==> rendering"
# A throwaway --config keeps whatever the developer has in ~/.config/bava out of
# the shot; bava writes its defaults there on first use. The frame we keep is
# early — cavacore's autosens keeps pulling the scale down as the (unchanging)
# tone plays, so a late frame is a flat little strip along the bottom.
rm -f "$work/config.toml"
# 1280x720 on purpose: in headless renders the now-playing HUD is laid out for
# a 720p-high surface regardless of --height, so at 900p/1080p it drifts off the
# top or bottom edge. Comfortably above AppStream's 620px minimum either way.
"$bava" --input "$clip" --out "$work/clip.mp4" \
    --config "$work/config.toml" --mode bars-box \
    --width 1280 --height 720 --fps 60 --duration 6 --headless

echo "==> grabbing a frame"
ffmpeg -y -loglevel error -ss 2.6 -i "$work/clip.mp4" -frames:v 1 "$out"

echo "==> $out"
