#!/usr/bin/env bash
# The repository PR entry point: verify the candidate before publishing it.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

preflight_args=()
separator=0
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--" ]; then
        separator=1
        shift
        break
    fi
    preflight_args+=("$1")
    shift
done
if [ "$separator" -ne 1 ]; then
    echo "Usage: scripts/create-pr.sh [preflight selectors and UI specs] -- [gh pr create options]" >&2
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

base_ref="$(node --input-type=module - "${preflight_args[@]}" <<'JS'
import { parseFlags } from './scripts/pr-platform/plan.mjs';
console.log(parseFlags(['--base', process.env.PATCH_COVERAGE_BASE ?? 'origin/main', ...process.argv.slice(2)]).base);
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

candidate="$(git rev-parse HEAD)"
"$repo_root/scripts/pr-preflight.sh" "${preflight_args[@]}"
if [ "$(git rev-parse HEAD)" != "$candidate" ] || [ -n "$(git status --porcelain)" ]; then
    echo "The candidate changed during preflight. Verify the new revision before creating its PR." >&2
    exit 1
fi
exec gh pr create --base "$pr_base" "$@"
