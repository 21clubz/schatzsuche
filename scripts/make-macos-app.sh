#!/usr/bin/env bash
#
# Wraps a built binary in a macOS application bundle, so the program can be
# started with a double-click instead of from a terminal.
#
# The bundle is what makes the window appear: the program decides between the
# terminal and the graphical interface by asking whether it sits in
# `Contents/MacOS`, so a loose binary gets the terminal and this gets a window.
#
#     scripts/make-macos-app.sh target/release/schatzsuche dist
#
# The signature is ad-hoc — the `-` identity. That is enough for the bundle to
# launch on the machine that built it, and NOT enough to pass Gatekeeper after
# a download. Notarisation needs a paid Apple Developer account; the README
# tells users how to open an unnotarised build instead.

set -euo pipefail

BINARY="${1:?usage: make-macos-app.sh <binary> <output-dir>}"
OUTDIR="${2:?usage: make-macos-app.sh <binary> <output-dir>}"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$(dirname "$0")/../Cargo.toml" | head -1)"
APP="$OUTDIR/Schatzsuche.app"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BINARY" "$APP/Contents/MacOS/Schatzsuche"
chmod +x "$APP/Contents/MacOS/Schatzsuche"
cp "$(dirname "$0")/../assets/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>Schatzsuche</string>
	<key>CFBundleDisplayName</key><string>Schatzsuche</string>
	<key>CFBundleIdentifier</key><string>io.github.21clubz.schatzsuche</string>
	<key>CFBundleVersion</key><string>$VERSION</string>
	<key>CFBundleShortVersionString</key><string>$VERSION</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleExecutable</key><string>Schatzsuche</string>
	<key>CFBundleIconFile</key><string>AppIcon</string>
	<key>LSMinimumSystemVersion</key><string>11.0</string>
	<key>NSHighResolutionCapable</key><true/>
	<key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

echo "built $APP (version $VERSION)"
