#!/usr/bin/env bash
# Real loopback TCP/Noise convergence at three group sizes, plus UDP discovery
# recovery and whole-file replica eviction. Run from any directory.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

for members in 5 20 50; do
    MESH_TALK_CHANNEL_MEMBERS="$members" cargo test -p mesh-talk-core --lib \
        channel_real_tcp_scale_rehearsal --features fast-test-kdf \
        -- --ignored --nocapture
done

cargo test -p mesh-talk-core --test channel_peer_recovery \
    -- --ignored --nocapture
cargo test -p mesh-talk-core --lib \
    channel_file_budget_keeps_recent_complete_files_and_evicts_older_ones \
    --features fast-test-kdf
