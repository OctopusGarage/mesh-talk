#!/usr/bin/env bash
# PreToolUse(Bash) guard for mesh-talk.
#
# Compilation and concurrent crypto/network tests can occupy available cores.
# Test builds use cheap KDF parameters, but heavy cargo work can still
# compete with interactive applications even under the configured job limits.
#
# `.cargo/config.toml` already caps `jobs` and `RUST_TEST_THREADS`, but that
# does not lower scheduling priority. This hook additionally requires heavy
# cargo commands to run under `nice` so they never starve interactive work.
#
# Behavior: if a Bash command invokes `cargo (build|test|clippy|run|check)` and
# is NOT already wrapped in `nice`, deny it with guidance. Everything else is
# allowed.
set -euo pipefail

input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // ""')"

# Only gate heavy cargo subcommands (allow fmt/metadata/tree/add/etc.).
#
# `cargo` must sit at a command position — start of line or right after a
# separator (; & | && || `( `), allowing leading ENV=val assignments — so that
# `cargo` appearing inside a quoted argument (e.g. `grep 'cargo test'`) or after
# another command (`echo cargo test`) does NOT trigger the guard.
if printf '%s' "$cmd" \
  | grep -Eq '(^|[;&|(]|&&|\|\|)[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]+[[:space:]]+)*cargo[[:space:]]+([+][^[:space:]]+[[:space:]]+)?(build|test|clippy|run|check)\b'; then
  # Allow when already scheduled with `nice` (word-bounded so "niceapp" etc.
  # do not count).
  if printf '%s' "$cmd" | grep -Eq '(^|[[:space:];&|(])nice([[:space:]]|$)'; then
    exit 0
  fi
  reason='CPU guard: heavy cargo commands must run under nice so they do not saturate the CPU. Re-run prefixed with: RUST_TEST_THREADS=4 nice -n 10 <your cargo command>. (.cargo/config.toml already caps jobs and test-threads; nice keeps the machine responsive. For the full suite use: nice -n 10 cargo test -- --test-threads=2)'
  jq -n --arg r "$reason" '{
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason: $r
    }
  }'
  exit 0
fi

exit 0
