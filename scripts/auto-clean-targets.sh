#!/usr/bin/env bash
set -euo pipefail

# Daily, unattended garbage collection for Mesh Talk Cargo build outputs.
# Only roots with this project's workspace metadata are eligible.
SCAN_ROOT="${HOME}/programming"
DRY_RUN=0
YES=0
MIN_GB="${MT_TARGET_GC_MIN_GB:-2}"
IDLE_DAYS="${MT_TARGET_GC_IDLE_DAYS:-7}"
LARGE_GB="${MT_TARGET_GC_LARGE_GB:-40}"
LARGE_IDLE_DAYS="${MT_TARGET_GC_LARGE_IDLE_DAYS:-2}"

usage() {
    cat <<'USAGE'
Usage: scripts/auto-clean-targets.sh [--scan-root PATH] [--yes] [--dry-run]

Remove dormant Mesh Talk Cargo target/ directories. Defaults: at least 2 GiB
and no file writes for 7 days, or at least 40 GiB and idle for 2 days.
An active cargo/rustc process prevents cleanup. Set MT_TARGET_CLEAN=0 to disable.
Thresholds can be changed with MT_TARGET_GC_MIN_GB, MT_TARGET_GC_IDLE_DAYS,
MT_TARGET_GC_LARGE_GB, and MT_TARGET_GC_LARGE_IDLE_DAYS.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --scan-root) SCAN_ROOT="${2:?missing scan root}"; shift 2 ;;
        --yes) YES=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        --help|-h) usage; exit 0 ;;
        *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ "${MT_TARGET_CLEAN:-1}" = 0 ]; then
    echo "Mesh Talk target cleanup disabled by MT_TARGET_CLEAN=0"
    exit 0
fi

for number in "$MIN_GB" "$IDLE_DAYS" "$LARGE_GB" "$LARGE_IDLE_DAYS"; do
    case "$number" in ''|*[!0-9]*) echo "Cleanup thresholds must be nonnegative integers" >&2; exit 2 ;; esac
done

if [ ! -d "$SCAN_ROOT" ]; then
    echo "Scan root does not exist: $SCAN_ROOT" >&2
    exit 2
fi
SCAN_ROOT="$(cd "$SCAN_ROOT" && pwd -P)"

build_running() {
    # `comm` contains executable names, not arguments from this script's own command line.
    ps -Ao comm= | awk '{ name=$0; sub(/^.*\//, "", name); if (name == "cargo" || name == "cargo-tauri" || name == "cargo-clippy" || name == "rustc") found=1 } END { exit !found }'
}

if build_running; then
    echo "Cargo, Tauri, or rustc is running; skipping target cleanup."
    exit 0
fi

is_mesh_talk_root() {
    local root="$1"
    [ -f "$root/Cargo.toml" ] && [ -f "$root/src-tauri/Cargo.toml" ] &&
        [ -d "$root/frontend" ] &&
        grep -Fq 'repository = "https://github.com/OctopusGarage/mesh-talk"' "$root/Cargo.toml" &&
        grep -Eq '^name = "mesh-talk"$' "$root/src-tauri/Cargo.toml"
}

clean_target() {
    local target="$1" root size_kb size_gb idle_days
    root="${target%/target}"
    is_mesh_talk_root "$root" || return 0

    # Keep recent outputs even when the directory itself has an old mtime.
    # Check this before du so active checkouts are cheap to skip.
    if [ -n "$(find "$target" -type f -mmin "-$(( LARGE_IDLE_DAYS * 1440 ))" -print -quit)" ]; then
        echo "Keeping recently used target: $target"
        return 0
    fi

    size_kb="$(du -sk "$target" | awk '{print $1}')"
    size_gb="$(( (size_kb + 1048575) / 1048576 ))"
    if [ "$size_kb" -ge "$(( LARGE_GB * 1048576 ))" ]; then
        idle_days="$LARGE_IDLE_DAYS"
    elif [ "$size_kb" -ge "$(( MIN_GB * 1048576 ))" ]; then
        idle_days="$IDLE_DAYS"
    else
        return 0
    fi
    if [ -n "$(find "$target" -type f -mmin "-$(( idle_days * 1440 ))" -print -quit)" ]; then
        echo "Keeping target used within ${idle_days} days: $target (${size_gb} GiB)"
        return 0
    fi

    if [ "$DRY_RUN" = 1 ]; then
        echo "DRY-RUN: remove $target (${size_gb} GiB, idle at least ${idle_days} days)"
        return 0
    fi
    if [ "$YES" != 1 ]; then
        printf 'Remove %s (%s GiB)? [y/N] ' "$target" "$size_gb"
        read -r answer
        case "$answer" in y|Y|yes|YES) ;; *) echo "Skipped $target"; return 0 ;; esac
    fi
    # Recheck after size calculation and prompt, before an unattended destructive operation.
    if build_running || [ -n "$(find "$target" -type f -mmin "-$(( idle_days * 1440 ))" -print -quit)" ]; then
        echo "Target became active; keeping $target"
        return 0
    fi
    echo "Removing dormant Mesh Talk build cache: $target (${size_gb} GiB)"
    rm -rf -- "$target"
}

# Prune unrelated heavyweight trees, and only inspect real directories (no symlinks).
while IFS= read -r -d '' target; do
    clean_target "$target"
done < <(find "$SCAN_ROOT" -type d \( -name .git -o -name node_modules -o -name .venv -o -name .stryker-tmp \) -prune -o -type d -name target -print0 -prune)
