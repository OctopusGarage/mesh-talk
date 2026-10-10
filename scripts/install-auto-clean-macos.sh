#!/usr/bin/env bash
set -euo pipefail

if [ "$(uname -s)" != Darwin ]; then
    echo "The daily scheduler uses macOS launchd; run auto-clean-targets.sh manually on this platform."
    exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
PLIST="$HOME/Library/LaunchAgents/org.octopusgarage.mesh-talk.target-gc.plist"
SCAN_ROOT="${MT_TARGET_GC_SCAN_ROOT:-$HOME/programming}"
mkdir -p "$(dirname "$PLIST")" "$HOME/Library/Logs"

python3 - "$PLIST" "$ROOT/scripts/auto-clean-targets.sh" "$SCAN_ROOT" "$HOME" <<'PY'
import plistlib
import sys

plist_path, script_path, scan_root, home = sys.argv[1:]
job = {
    "Label": "org.octopusgarage.mesh-talk.target-gc",
    "ProgramArguments": ["/bin/bash", script_path, "--scan-root", scan_root, "--yes"],
    "StartCalendarInterval": {"Hour": 3, "Minute": 0},
    "RunAtLoad": False,
    "StandardOutPath": home + "/Library/Logs/mesh-talk-target-gc.log",
    "StandardErrorPath": home + "/Library/Logs/mesh-talk-target-gc.log",
}
with open(plist_path, "wb") as output:
    plistlib.dump(job, output)
PY

LABEL="org.octopusgarage.mesh-talk.target-gc"
DOMAIN="gui/$(id -u)"
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
launchctl bootstrap "$DOMAIN" "$PLIST"
echo "Daily Mesh Talk target cleanup installed for 03:00 local time."
echo "Preview: $ROOT/scripts/auto-clean-targets.sh --scan-root '$SCAN_ROOT' --dry-run"
echo "Disable: launchctl bootout '$DOMAIN/$LABEL'"
