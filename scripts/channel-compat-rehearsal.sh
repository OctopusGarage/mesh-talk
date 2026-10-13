#!/usr/bin/env bash
# Build the last pre-replica CLI and exercise it against the current CLI.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/mesh-talk-compat.XXXXXX")"
cleanup() {
    git -C "$repo_root" worktree remove --force "$tmp_dir/old" >/dev/null 2>&1 || true
    rm -rf "$tmp_dir"
}
trap cleanup EXIT

git -C "$repo_root" worktree add --detach "$tmp_dir/old" b1e6486
CARGO_TARGET_DIR="$tmp_dir/target" cargo build \
    --manifest-path "$tmp_dir/old/Cargo.toml" \
    -p mesh-talk-core --bin mesh-talk-node --features fast-test-kdf
cp "$tmp_dir/target/debug/mesh-talk-node" "$tmp_dir/mesh-talk-node-old"

cd "$repo_root"
MESH_TALK_OLD_CLI="$tmp_dir/mesh-talk-node-old" cargo test \
    -p mesh-talk-core --test channel_mixed_version --features fast-test-kdf \
    -- --ignored --nocapture
