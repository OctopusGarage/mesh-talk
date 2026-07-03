#!/usr/bin/env bash
set -euo pipefail

ROOT="$(pwd)"
ASSUME_YES=0
DRY_RUN=0

usage() {
    cat <<'USAGE'
Usage: scripts/clean-target-cache.sh [--root PATH] [--yes] [--dry-run]

Conservative cleanup for Cargo target artifacts.

Environment:
  MT_TARGET_CLEAN=0              Disable cleanup.
  MT_TARGET_CLEAN_MAX_GB=40      Run only when target/ is at least this large.
  MT_TARGET_CLEAN_DAYS=14        Remove incremental/deps artifacts older than this.
  MT_TARGET_CLEAN_DOCS=0         Also remove generated target/doc when set to 1.
  MT_TARGET_CLEAN_INTERVAL_HOURS=24
                                  Skip automatic cleanup if it ran recently.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --root)
            ROOT="${2:-}"
            shift 2
            ;;
        --yes|-y)
            ASSUME_YES=1
            shift
            ;;
        --dry-run|-n)
            DRY_RUN=1
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ "${MT_TARGET_CLEAN:-1}" = "0" ]; then
    echo "Cargo target cleanup disabled by MT_TARGET_CLEAN=0"
    exit 0
fi

cd "$ROOT"

if [ ! -f "Makefile" ] || [ ! -d "src-tauri" ] || [ ! -d "frontend" ]; then
    echo "Please run this script from the project root directory" >&2
    exit 1
fi

if [ ! -d "target" ]; then
    echo "No target/ directory found; nothing to clean."
    exit 0
fi

MAX_GB="${MT_TARGET_CLEAN_MAX_GB:-40}"
DAYS="${MT_TARGET_CLEAN_DAYS:-14}"
CLEAN_DOCS="${MT_TARGET_CLEAN_DOCS:-0}"
INTERVAL_HOURS="${MT_TARGET_CLEAN_INTERVAL_HOURS:-24}"
STAMP="target/.mesh-talk-clean-target-cache.stamp"

TARGET_KB="$(du -sk target 2>/dev/null | awk '{print $1}')"
TARGET_GB="$(( (TARGET_KB + 1024 * 1024 - 1) / (1024 * 1024) ))"

if [ "$TARGET_GB" -lt "$MAX_GB" ]; then
    echo "target/ is ${TARGET_GB}G; below ${MAX_GB}G threshold. Nothing to clean."
    exit 0
fi

if [ "$DRY_RUN" != "1" ] && [ -f "$STAMP" ]; then
    now="$(date +%s)"
    last="$(stat -f %m "$STAMP" 2>/dev/null || stat -c %Y "$STAMP" 2>/dev/null || echo 0)"
    interval_seconds="$(( INTERVAL_HOURS * 60 * 60 ))"
    if [ "$(( now - last ))" -lt "$interval_seconds" ]; then
        echo "Cargo target cleanup ran recently; skipping until ${INTERVAL_HOURS}h interval expires."
        exit 0
    fi
fi

run_or_print() {
    if [ "$DRY_RUN" = "1" ]; then
        printf 'DRY-RUN:'
        printf ' %q' "$@"
        printf '\n'
    else
        "$@"
    fi
}

remove_old_entries() {
    local dir="$1"
    local maxdepth="$2"
    shift 2

    [ -d "$dir" ] || return 0

    find "$dir" -maxdepth "$maxdepth" "$@" -mtime +"$DAYS" -print0 |
        while IFS= read -r -d '' entry; do
            run_or_print rm -rf "$entry"
        done
}

if [ "$ASSUME_YES" != "1" ] && [ "$DRY_RUN" != "1" ]; then
    echo "target/ is ${TARGET_GB}G. Clean Cargo caches older than ${DAYS} days? [y/N]"
    read -r answer
    case "$answer" in
        y|Y|yes|YES) ;;
        *)
            echo "Skipped."
            exit 0
            ;;
    esac
fi

echo "Cleaning Cargo target cache in $ROOT"
echo "Current target/ size: ${TARGET_GB}G; age threshold: ${DAYS} days"

# Incremental caches are the safest high-value cleanup target. Cargo recreates them.
remove_old_entries "target/debug/incremental" 1 -mindepth 1 -type d
remove_old_entries "target/release/incremental" 1 -mindepth 1 -type d
remove_old_entries "target/wasm32-unknown-unknown/debug/incremental" 1 -mindepth 1 -type d

# Very old debug artifacts are often stale hash variants from old feature/profile/test builds.
remove_old_entries "target/debug/deps" 1 -mindepth 1 -type f
remove_old_entries "target/debug/.fingerprint" 1 -mindepth 1 -type d
remove_old_entries "target/debug/build" 1 -mindepth 1 -type d

# Coverage output is generated on demand and does not help normal builds.
if [ -d "target/llvm-cov-target" ]; then
    run_or_print rm -rf "target/llvm-cov-target"
fi

# Generated rustdoc can be large and is cheap to regenerate, but leave it opt-in because docs
# may be useful during local API work.
if [ "$CLEAN_DOCS" = "1" ] && [ -d "target/doc" ]; then
    run_or_print rm -rf "target/doc"
fi

if [ "$DRY_RUN" = "1" ]; then
    echo "Dry run complete."
else
    touch "$STAMP"
    AFTER_KB="$(du -sk target 2>/dev/null | awk '{print $1}')"
    AFTER_GB="$(( (AFTER_KB + 1024 * 1024 - 1) / (1024 * 1024) ))"
    echo "Cleanup complete. target/ is now ${AFTER_GB}G."
fi
