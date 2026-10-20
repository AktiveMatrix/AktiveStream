#!/bin/sh
# Packages the release binary as a Developer ID-signed .app bundle so its TCC grants persist.
# Usage: make-bundle.sh <src_dir> <info_plist> <out_app>
set -e
SRC="$1"
INFO_PLIST="$2"
APP="$3"
BIN="$SRC/.build/release/aktivestream-host"
IDENTITY="Developer ID Application: Your Name (TEAMID)"

test -f "$BIN" || { echo "missing release binary: $BIN"; exit 1; }

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/aktivestream-host"
cp "$INFO_PLIST" "$APP/Contents/Info.plist"

# Sign the executable, then the bundle (inside-out), with the hardened runtime and a timestamp.
codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP/Contents/MacOS/aktivestream-host"
codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP"

echo "=== verify ==="
codesign --verify --strict --verbose=2 "$APP"
codesign -dvv "$APP" 2>&1 | grep -E "Identifier=|TeamIdentifier=|Authority=" | head -4
echo "=== gatekeeper ==="
spctl -a -vv -t exec "$APP" 2>&1 | head -3 || true
