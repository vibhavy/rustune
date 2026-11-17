#!/bin/sh
# Renders assets/icon.svg into the PNGs and macOS .icns used by the app.
# Needs: rsvg-convert (brew install librsvg), sips + iconutil (built into macOS).
set -eu
cd "$(dirname "$0")"
rsvg-convert -w 1024 -h 1024 icon.svg -o icon.png
rsvg-convert -w 512 -h 512 icon.svg -o icon-512.png   # embedded as the window/Dock icon
rsvg-convert -w 44 -h 44 tray.svg -o tray.png          # menu bar icon (22 pt @2x)
set="$(mktemp -d)/rustune.iconset"
mkdir -p "$set"
for s in 16 32 128 256 512; do
  rsvg-convert -w "$s" -h "$s" icon.svg -o "$set/icon_${s}x${s}.png"
  rsvg-convert -w $((s * 2)) -h $((s * 2)) icon.svg -o "$set/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$set" -o rustune.icns
rm -rf "$(dirname "$set")"
echo "wrote icon.png, icon-512.png, tray.png, rustune.icns"
