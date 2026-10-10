#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCAN="$(mktemp -d)"
trap 'rm -rf "$SCAN"' EXIT
SCAN="$(cd "$SCAN" && pwd -P)"
mkdir -p "$SCAN/bin"
printf '#!/bin/sh\nexit 0\n' > "$SCAN/bin/ps"
chmod +x "$SCAN/bin/ps"
export PATH="$SCAN/bin:$PATH"

for name in old recent unrelated; do
    mkdir -p "$SCAN/$name/target/debug" "$SCAN/$name/src-tauri" "$SCAN/$name/frontend"
    touch "$SCAN/$name/target/debug/artifact"
done
for name in old recent; do
    printf '[workspace.package]\nrepository = "https://github.com/OctopusGarage/mesh-talk"\n' > "$SCAN/$name/Cargo.toml"
    printf '[package]\nname = "mesh-talk"\n' > "$SCAN/$name/src-tauri/Cargo.toml"
done
printf '[workspace]\n' > "$SCAN/unrelated/Cargo.toml"
touch -t 202401010000 "$SCAN/old/target/debug/artifact" "$SCAN/unrelated/target/debug/artifact"

export MT_TARGET_GC_MIN_GB=0 MT_TARGET_GC_LARGE_GB=1000 MT_TARGET_GC_IDLE_DAYS=1 MT_TARGET_GC_LARGE_IDLE_DAYS=1
printf '#!/bin/sh\necho /usr/bin/cargo\n' > "$SCAN/bin/ps"
chmod +x "$SCAN/bin/ps"
"$ROOT/scripts/auto-clean-targets.sh" --scan-root "$SCAN" --yes >/dev/null
[ -e "$SCAN/old/target/debug/artifact" ] || { echo "Active build guard failed" >&2; exit 1; }
printf '#!/bin/sh\nexit 0\n' > "$SCAN/bin/ps"
chmod +x "$SCAN/bin/ps"
output="$("$ROOT/scripts/auto-clean-targets.sh" --scan-root "$SCAN" --dry-run)"
case "$output" in *"DRY-RUN: remove $SCAN/old/target"*) ;; *) echo "Old Mesh Talk target was not selected" >&2; exit 1 ;; esac
case "$output" in *"DRY-RUN: remove $SCAN/recent/target"*|*"DRY-RUN: remove $SCAN/unrelated/target"*) echo "Unsafe target selected" >&2; exit 1 ;; esac

"$ROOT/scripts/auto-clean-targets.sh" --scan-root "$SCAN" --yes
[ ! -e "$SCAN/old/target" ] || { echo "Old target was not removed" >&2; exit 1; }
[ -e "$SCAN/recent/target/debug/artifact" ] || { echo "Recent target was removed" >&2; exit 1; }
[ -e "$SCAN/unrelated/target/debug/artifact" ] || { echo "Unrelated target was removed" >&2; exit 1; }
echo "auto-clean-targets fixture test passed"
