#!/bin/sh
# mkicns.sh DRAWING.svg OUT.icns WORKDIR -- a Mac's icon file from a drawing, with the tools
# every Mac has: qlmanage draws the SVG once, large; sips cuts the ten sizes an icon is made
# of; iconutil binds them. Exit 1, with nothing written, if any of the three will not.
set -eu
svg=$1; out=$2; work=$3
name=$(basename "$svg" .svg)
mkdir -p "$work/$name.iconset"
qlmanage -t -s 1024 -o "$work" "$svg" >/dev/null 2>&1
for s in 16 32 128 256 512; do
    sips -z $s $s "$work/$name.svg.png" --out "$work/$name.iconset/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) "$work/$name.svg.png" --out "$work/$name.iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$work/$name.iconset" -o "$out"
