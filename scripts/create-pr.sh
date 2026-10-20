#!/usr/bin/env bash
# Publish promptly after cheap checks; comprehensive validation runs in PR CI.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

preflight_args=()
full_preflight=0
separator=0
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--" ]; then
        separator=1
        shift
        break
    fi
    if [ "$1" = "--full-preflight" ]; then
        full_preflight=1
        shift
        continue
    fi
    preflight_args+=("$1")
    shift
done
if [ "$separator" -ne 1 ]; then
    echo "Usage: scripts/create-pr.sh [--full-preflight] [preflight selectors and UI specs] -- [gh pr create options]" >&2
    exit 2
fi
if [ -n "${GH_REPO:-}" ]; then
    echo "Unset GH_REPO: verified PR creation uses this checkout's repository." >&2
    exit 2
fi
for arg in "$@"; do
    case "$arg" in
        --head|--head=*|-H*|--repo|--repo=*|-R*|--base|--base=*|-B*)
            echo "Head/repository overrides are forbidden. Select the shared comparison/PR base before --." >&2
            exit 2
            ;;
    esac
done

# Bash 3.2 treats an empty array as unset under nounset.
base_ref="$(node --input-type=module - ${preflight_args[@]+"${preflight_args[@]}"} <<'JS'
import { parseFlags } from './scripts/pr-platform/plan.mjs';
const flags = parseFlags(['--base', process.env.PATCH_COVERAGE_BASE ?? 'origin/main', ...process.argv.slice(2)]);
if (flags.planOnly) throw Error('Preview with scripts/pr-platform/run.mjs --plan; creation publishes a PR.');
console.log(flags.base);
JS
)"
case "$base_ref" in
    origin/*) pr_base="${base_ref#origin/}" ;;
    *)
        echo "The PR base must be an origin/<branch> tracking ref, not a local revision." >&2
        exit 2
        ;;
esac
git check-ref-format --branch "$pr_base" >/dev/null

base_sha="$(git rev-parse --verify "$base_ref")"
candidate="$(git rev-parse HEAD)"
if [ -n "$(git status --porcelain)" ]; then
    echo "Commit or stash changes before creating a PR." >&2
    exit 1
fi
git diff --check "$base_ref"...HEAD
if [ "$full_preflight" -eq 1 ]; then
    "$repo_root/scripts/prepare-pr.sh" ${preflight_args[@]+"${preflight_args[@]}"}
else
    node scripts/check-repository-hygiene.mjs
    node scripts/validation/standards.mjs
    echo "Fast PR creation: full platform, browser and native checks run in CI. Merge only after CI passes."
fi
if [ "$(git rev-parse HEAD)" != "$candidate" ] || [ -n "$(git status --porcelain)" ] || [ "$(git rev-parse --verify "$base_ref")" != "$base_sha" ]; then
    echo "The candidate or comparison base changed during PR checks. Retry with the committed revision." >&2
    exit 1
fi
exec gh pr create --base "$pr_base" "$@"
