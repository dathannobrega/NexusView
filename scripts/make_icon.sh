#!/usr/bin/env bash
#
# Generate the app icon (AppIcon.icns) from the source SVG.
#
#   ./scripts/make_icon.sh [path/to/icon.svg]
#
# Rasterizes every required size via AppKit (NSImage), then packs them with
# iconutil. Output: app/Resources/AppIcon.icns (committed; used by build_app.sh).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SVG="${1:-$ROOT/app/Resources/AppIcon.svg}"
ICONSET="$ROOT/build/AppIcon.iconset"
ICNS="$ROOT/app/Resources/AppIcon.icns"

[ -f "$SVG" ] || { echo "error: SVG not found: $SVG" >&2; exit 1; }

rm -rf "$ICONSET"
mkdir -p "$ICONSET" "$(dirname "$ICNS")"

echo "==> Rasterizing $SVG"
swift "$ROOT/scripts/rasterize_icon.swift" "$SVG" "$ICONSET"

echo "==> Packing .icns"
iconutil -c icns "$ICONSET" -o "$ICNS"

echo "==> Done: $ICNS ($(du -h "$ICNS" | cut -f1))"
