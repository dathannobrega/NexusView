#!/usr/bin/env bash
#
# trust_local.sh — let a locally-installed NexusView open files without the
# macOS Gatekeeper "could not verify … is free of malware" prompt.
#
# Why this is needed: the public .dmg is **ad-hoc signed, not notarized** (no
# paid Apple Developer ID). macOS quarantines downloaded apps AND downloaded
# documents; opening a quarantined document with a non-notarized app triggers
# the Gatekeeper prompt every single time. This script clears the quarantine
# flag from the installed app and from any files/folders you pass.
#
#   ./scripts/trust_local.sh                  # trust just the installed app
#   ./scripts/trust_local.sh ~/Downloads      # also clear a folder of evidence
#   ./scripts/trust_local.sh a.csv b.csv dir/ # clear specific paths
#
# It only removes the `com.apple.quarantine` xattr from the paths you name — it
# does NOT disable Gatekeeper system-wide. The proper fix is Developer-ID
# signing + notarization in CI (see the "Distribution" note in release.yml).
set -euo pipefail

APP="${NEXUSVIEW_APP:-/Applications/NexusView.app}"

if [ -d "$APP" ]; then
  xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true
  echo "✓ trusted app: $APP"
else
  echo "note: $APP not found — pass NEXUSVIEW_APP=/path/to/NexusView.app if it lives elsewhere"
fi

for p in "$@"; do
  if [ -e "$p" ]; then
    xattr -dr com.apple.quarantine "$p" 2>/dev/null || true
    echo "✓ de-quarantined: $p"
  else
    echo "skip (not found): $p"
  fi
done

echo "Done — NexusView opens those files without the Gatekeeper prompt now."
