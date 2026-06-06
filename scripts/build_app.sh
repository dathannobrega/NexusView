#!/usr/bin/env bash
#
# Build the NexusView engine + app and assemble a double-clickable .app bundle.
#
#   ./scripts/build_app.sh [--debug]
#
# Output: build/NexusView.app
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG="release"
[[ "${1:-}" == "--debug" ]] && CONFIG="debug"

export PATH="$HOME/.cargo/bin:$PATH"

echo "==> Building Rust engine (release static lib)"
( cd "$ROOT/engine" && cargo build --release -p nexus-ffi )

echo "==> Syncing FFI header into the Swift C module"
cp "$ROOT/engine/nexus-ffi/include/nexusview.h" "$ROOT/app/Sources/CNexusEngine/nexusview.h"

echo "==> Building Swift app ($CONFIG)"
SWIFT_FLAGS=()
[[ "$CONFIG" == "release" ]] && SWIFT_FLAGS+=(-c release)
( cd "$ROOT/app" && swift build "${SWIFT_FLAGS[@]}" )

BIN="$(cd "$ROOT/app" && swift build "${SWIFT_FLAGS[@]}" --show-bin-path)/NexusView"

echo "==> Assembling NexusView.app"
APP="$ROOT/build/NexusView.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/NexusView"
cp "$ROOT/app/Resources/Info.plist" "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
if [ -f "$ROOT/app/Resources/AppIcon.icns" ]; then
  cp "$ROOT/app/Resources/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
fi

echo "==> Ad-hoc code signing"
codesign --force --deep --sign - "$APP" 2>/dev/null \
  && codesign --verify --verbose=1 "$APP" 2>&1 | sed 's/^/    /' \
  || echo "    (codesign unavailable — app still runs locally; right-click ▸ Open if Gatekeeper warns)"

echo "==> Done: $APP"
echo "    Launch:  open '$APP'"
echo "    Or:      open -a '$APP' samples/incident_sample.csv"
