#!/usr/bin/env bash
# The common gate for creating a PR and preparing updates to an existing PR.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
if [ "$(uname -s)" = Darwin ] && [ "${MESH_PR_KEEP_AWAKE:-0}" != 1 ]; then
    # Preparation includes full health after platform preflight. Keep its browser
    # and workspace gates awake too; release the assertion with this child.
    exec caffeinate -is env MESH_PR_KEEP_AWAKE=1 bash "$repo_root/scripts/prepare-pr.sh" "$@"
fi
if [ -n "$(git status --porcelain)" ]; then
    echo "Commit or stash changes before preparing a PR candidate." >&2
    exit 1
fi
node scripts/validation/standards.mjs
candidate="$(git rev-parse HEAD)"
base_ref="$(node --input-type=module - "$@" <<'JS'
import { parseFlags } from './scripts/pr-platform/plan.mjs';
const flags = parseFlags(['--base', process.env.PATCH_COVERAGE_BASE ?? 'origin/main', ...process.argv.slice(2)]);
if (flags.planOnly) throw Error('Preparation requires execution; preview with scripts/pr-platform/run.mjs --plan.');
console.log(flags.base);
JS
)"
base_sha="$(git rev-parse --verify "$base_ref")"
# Fail on invalid selectors or unavailable required native evidence before builds.
node scripts/pr-platform/run.mjs --plan --base "$base_ref" "$@" >/dev/null
node scripts/validation/pr-scenarios.mjs verify "$base_ref"
# Catch selected platform failures before starting comprehensive suites.
env MESH_VALIDATION_PREFLIGHT=0 "$repo_root/scripts/pr-preflight.sh" "$@"
# Full health reuses its individual successful stages. FAST must never weaken
# publication evidence, even when inherited from the developer's shell.
env FAST=0 bash "$repo_root/scripts/check-health.sh"
if [ "$(git rev-parse HEAD)" != "$candidate" ] || [ -n "$(git status --porcelain)" ] || [ "$(git rev-parse --verify "$base_ref")" != "$base_sha" ]; then
    echo "Candidate or comparison base changed during preparation. Verify the new revision." >&2
    exit 1
fi
echo "PR preparation passed for $candidate against $base_ref ($base_sha)."
