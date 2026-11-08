#!/usr/bin/env bash
# All compiler-cohort consumers independently derive the pinned coverage env.
set -euo pipefail
mode="${1:-}"
case "$mode" in
    produce|default|ignored) shift ;;
    *) echo 'Usage: linux-workspace.sh produce|default|ignored ARGS...' >&2; exit 2 ;;
esac
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
export CARGO_TARGET_DIR="$root/target/llvm-cov-target"
# Assignment preserves show-env failure; eval of a failed substitution does not.
coverage_environment="$(cargo llvm-cov show-env --sh)"
eval "$coverage_environment"
ulimit -c 262144
case "$mode" in
    produce) node scripts/ci/windows-coverage.mjs produce-linux-workspace "$@" ;;
    default) node scripts/ci/windows-coverage.mjs consume-linux-workspace "$@" ;;
    ignored) node scripts/evals/backend-runner.mjs --archive "$@" ;;
esac
