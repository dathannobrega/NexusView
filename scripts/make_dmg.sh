#!/usr/bin/env bash
#
# Build NexusView.app and package it into a drag-to-install .dmg.
#
#   ./scripts/make_dmg.sh [VERSION]
#
# VERSION defaults to the workspace version in engine/Cargo.toml. Requires
# `create-dmg` (brew install create-dmg). Output: build/NexusView-<version>.dmg
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-$(awk -F'"' '/^version = /{print $2; exit}' "$ROOT/engine/Cargo.toml" 2>/dev/null || true)}"
VERSION="${VERSION:-0.1.0}"

# create-dmg gives a polished window layout (icon positions, background). It is
# optional: if it is missing, or if its Finder/AppleScript step fails (common on
# a headless CI runner), we fall back to a plain hdiutil DMG below.
HAVE_CREATE_DMG=0
command -v create-dmg >/dev/null 2>&1 && HAVE_CREATE_DMG=1

echo "==> Building NexusView.app"
"$ROOT/scripts/build_app.sh"

APP="$ROOT/build/NexusView.app"

echo "==> Stamping bundle version $VERSION"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" "$APP/Contents/Info.plist" 2>/dev/null || true
codesign --force --deep --sign - "$APP" 2>/dev/null || true

DMG="$ROOT/build/NexusView-$VERSION.dmg"
rm -f "$DMG"

# Optional background art (an arrow "drag here"). The DMG works without it.
BG_ARGS=()
if [ -f "$ROOT/app/Resources/dmg-background.png" ]; then
  BG_ARGS=(--background "$ROOT/app/Resources/dmg-background.png")
fi

echo "==> Building drag-to-install DMG"
# Layout: the app on the left, an Applications alias on the right — the user
# drags one onto the other to install. --app-drop-link creates the symlink.
#
# The ${BG_ARGS[@]+"${BG_ARGS[@]}"} guard expands to nothing when BG_ARGS is
# empty instead of tripping `set -u` (nounset) — the system bash on macOS
# (and on GitHub's macOS runners) is 3.2, where a bare "${BG_ARGS[@]}" on an
# empty array is an "unbound variable" error.
if [ "$HAVE_CREATE_DMG" = 1 ]; then
  create-dmg \
    --volname "NexusView $VERSION" \
    --window-pos 200 120 \
    --window-size 640 400 \
    --icon-size 120 \
    --icon "NexusView.app" 160 185 \
    --hide-extension "NexusView.app" \
    --app-drop-link 480 185 \
    --no-internet-enable \
    ${BG_ARGS[@]+"${BG_ARGS[@]}"} \
    "$DMG" "$APP" || true   # create-dmg can exit non-zero after a successful detach retry
fi

# Fallback: create-dmg drives Finder/AppleScript to lay out the window, which can
# fail on a headless CI runner (no Finder session). If no DMG was produced — or
# create-dmg isn't installed — build a plain but fully functional drag-to-install
# DMG with hdiutil: the .app plus an /Applications symlink to drop it onto.
if [ ! -f "$DMG" ]; then
  echo "==> create-dmg unavailable or produced no DMG; falling back to hdiutil"
  STAGE="$(mktemp -d)"
  cp -R "$APP" "$STAGE/NexusView.app"
  ln -s /Applications "$STAGE/Applications"
  hdiutil create \
    -volname "NexusView $VERSION" \
    -srcfolder "$STAGE" \
    -fs HFS+ \
    -format UDZO \
    -ov "$DMG" >/dev/null
  rm -rf "$STAGE"
fi

if [ ! -f "$DMG" ]; then
  echo "error: DMG was not produced" >&2
  exit 1
fi

echo "==> Done: $DMG ($(du -h "$DMG" | cut -f1))"
