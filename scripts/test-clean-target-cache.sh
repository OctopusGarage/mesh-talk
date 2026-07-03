#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/clean-target-cache.sh"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

mkdir -p "$TMPDIR/target/debug/incremental/old-cache"
mkdir -p "$TMPDIR/target/debug/incremental/new-cache"
mkdir -p "$TMPDIR/target/llvm-cov-target/debug"
mkdir -p "$TMPDIR/target/doc/src"
mkdir -p "$TMPDIR/target/debug/deps"
mkdir -p "$TMPDIR/src-tauri" "$TMPDIR/frontend"
touch "$TMPDIR/Makefile"
touch "$TMPDIR/target/debug/incremental/old-cache/state.bin"
touch "$TMPDIR/target/debug/incremental/new-cache/state.bin"
touch "$TMPDIR/target/llvm-cov-target/debug/profile.profraw"
touch "$TMPDIR/target/doc/src/index.html"
touch "$TMPDIR/target/debug/deps/libfresh.rlib"

find "$TMPDIR/target/debug/incremental/old-cache" -exec touch -t 202401010000 {} +

MT_TARGET_CLEAN_DAYS=30 \
MT_TARGET_CLEAN_MAX_GB=0 \
MT_TARGET_CLEAN_DOCS=1 \
"$SCRIPT" --root "$TMPDIR" --yes

if [ -e "$TMPDIR/target/debug/incremental/old-cache" ]; then
    echo "old incremental cache was not removed" >&2
    exit 1
fi

if [ ! -e "$TMPDIR/target/debug/incremental/new-cache/state.bin" ]; then
    echo "new incremental cache was removed unexpectedly" >&2
    exit 1
fi

if [ -e "$TMPDIR/target/llvm-cov-target" ]; then
    echo "coverage target cache was not removed" >&2
    exit 1
fi

if [ -e "$TMPDIR/target/doc" ]; then
    echo "generated docs were not removed" >&2
    exit 1
fi

if [ ! -e "$TMPDIR/target/debug/deps/libfresh.rlib" ]; then
    echo "fresh deps artifact was removed unexpectedly" >&2
    exit 1
fi

echo "clean-target-cache fixture test passed"
