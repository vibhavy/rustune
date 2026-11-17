#!/bin/sh
# Builds rustune.app (release binary + icon + Info.plist), ad-hoc signs it, and
# copies it to dist/ or, with --install, to /Applications.
#
#   scripts/bundle_macos.sh            # -> dist/rustune.app
#   scripts/bundle_macos.sh --install  # -> /Applications/rustune.app
#
# The bundle is assembled in a temp folder on the system disk: on non-APFS/HFS
# volumes macOS adds "._*" sidecar files, which break the code signature.
set -eu
cd "$(dirname "$0")/.."

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
STAGE="$(mktemp -d)"
APP="$STAGE/rustune.app"
trap 'rm -rf "$STAGE"' EXIT

cargo build --release

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp -X "$TARGET_DIR/release/rustune" "$APP/Contents/MacOS/rustune"
cp -X assets/rustune.icns "$APP/Contents/Resources/rustune.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>rustune</string>
  <key>CFBundleDisplayName</key><string>rustune</string>
  <key>CFBundleIdentifier</key><string>dev.rustune.player</string>
  <key>CFBundleExecutable</key><string>rustune</string>
  <key>CFBundleIconFile</key><string>rustune</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <!-- The Web Playback SDK page is served from 127.0.0.1 (see src/sdk.rs). -->
  <key>NSAppTransportSecurity</key>
  <dict><key>NSAllowsLocalNetworking</key><true/></dict>
</dict>
</plist>
PLIST

# Ad-hoc signature: enough for an app built and run on this Mac.
codesign --force --sign - "$APP"
codesign --verify --strict "$APP"

if [ "${1:-}" = "--install" ]; then
  DEST=/Applications/rustune.app
else
  mkdir -p dist
  DEST=dist/rustune.app
fi
rm -rf "$DEST"
ditto --norsrc "$APP" "$DEST"
echo "rustune $VERSION -> $DEST"
