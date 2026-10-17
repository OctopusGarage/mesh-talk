#!/usr/bin/env bash
set -euo pipefail

if [ "$(uname -s)" != Darwin ]; then
    echo "Scheduled cleanup uses macOS launchd; run build-retention.py clean manually on this platform."
    exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
PLIST="$HOME/Library/LaunchAgents/org.octopusgarage.mesh-talk.target-gc.plist"
RUNNER="$HOME/Library/Application Support/Mesh Talk/build-retention.py"
mkdir -p "$(dirname "$PLIST")" "$HOME/Library/Logs"
mkdir -p "$(dirname "$RUNNER")"
cp "$ROOT/scripts/build-retention.py" "$RUNNER"

python3 - "$PLIST" "$(command -v python3)" "$RUNNER" "$ROOT" "$HOME" <<'PY'
import os
import plistlib
import sys

plist_path, python_path, runner, root, home = sys.argv[1:]
job = {
    "Label": "org.octopusgarage.mesh-talk.target-gc",
    "ProgramArguments": [python_path, runner, "--root", root, "clean"],
    "StartCalendarInterval": {"Hour": 3, "Minute": 0},
    # A busy build must defer cleanup, not postpone it until the following night.
    "StartInterval": 600,
    "RunAtLoad": False,
    "EnvironmentVariables": {
        key: value for key, value in os.environ.items()
        if key in {"MT_BUILD_STATE_DIR", "MT_BUILD_CHECKOUTS", "MT_BUILD_CACHE_GB", "MT_BUILD_KEEP", "MT_BUILD_SCAN_INTERVAL_SECONDS", "MT_TARGET_CLEAN", "PATH"}
    },
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
echo "Mesh Talk cleanup installed: retry every 10 minutes and run at 03:00 local time."
echo "Preview: python3 '$ROOT/scripts/build-retention.py' clean --dry-run"
echo "Disable: launchctl bootout '$DOMAIN/$LABEL'"
