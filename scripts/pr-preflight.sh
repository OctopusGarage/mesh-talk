#!/usr/bin/env bash
# Fast, change-focused feedback before opening a PR. Pass relevant Playwright specs
# when frontend rendering changed; the coverage report names lines still untested.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Keep headless macOS Docker/SSH checks out of maintenance-sleep cycles.
# caffeinate releases its assertions when this child exits; no settings change.
if [ "$(uname -s)" = Darwin ] && [ "${MESH_PR_KEEP_AWAKE:-0}" != 1 ]; then
    exec caffeinate -is env MESH_PR_KEEP_AWAKE=1 "$repo_root/scripts/pr-preflight.sh" "$@"
fi

if [ -n "$(git status --porcelain)" ]; then
    echo "Commit or stash changes before PR preflight so the diff matches the tested revision." >&2
    exit 1
fi

candidate="$(git rev-parse HEAD)"

base_ref="$(node --input-type=module - "$@" <<'JS'
import { parseFlags } from './scripts/pr-platform/plan.mjs';
console.log(parseFlags(['--base', process.env.PATCH_COVERAGE_BASE ?? 'origin/main', ...process.argv.slice(2)]).base);
JS
)"
git rev-parse --verify "$base_ref" >/dev/null
merge_base="$(git merge-base HEAD "$base_ref")"
changed_files="$(git diff --name-only "$merge_base" HEAD)"

# One parser owns both platform selectors and the existing positional UI specs.
ui_specs_text="$(node --input-type=module - "$@" <<'JS'
import { parseFlags } from './scripts/pr-platform/plan.mjs';
const flags = parseFlags(process.argv.slice(2));
if (flags.planOnly) throw new Error('Preview with node scripts/pr-platform/run.mjs --plan; PR preflight requires execution.');
console.log(flags.uiSpecs.join('\n'));
JS
)"
ui_specs=()
while IFS= read -r spec; do
    if [ -n "$spec" ]; then ui_specs+=("$spec"); fi
done <<< "$ui_specs_text"

# Validate selectors before local work, without starting either environment.
node scripts/pr-platform/run.mjs --plan --base "$base_ref" "$@" >/dev/null

node --test scripts/diagnostics/*.test.mjs
typos

if printf '%s\n' "$changed_files" | rg -q '^frontend/'; then
    (
        cd frontend
        npm run typecheck
        npm run lint
        npm run test:coverage
    )

    coverage_reports=(frontend/coverage/lcov.info)
    if [ "${#ui_specs[@]}" -gt 0 ]; then
        # JavaScript template literals belong to Node.
        # shellcheck disable=SC2016
        node --input-type=module -e 'import { readdir, unlink } from "node:fs/promises"; const dir="frontend/coverage/e2e-parts"; for (const name of await readdir(dir).catch(() => [])) if (name.endsWith(".json")) await unlink(`${dir}/${name}`);'
        (
            cd frontend
            MESH_TALK_E2E_COVERAGE=1 npx playwright test --project=chromium --workers=1 --retries=0 "${ui_specs[@]}"
            node scripts/merge-e2e-coverage.mjs
        )
        coverage_reports+=(frontend/coverage/e2e.lcov)
    fi
    PATCH_COVERAGE_BASE="$base_ref" node scripts/patch-coverage.mjs "${coverage_reports[@]}"
fi

if printf '%s\n' "$changed_files" | rg -q '^(frontend/e2e/portable-core\.spec\.ts|frontend/e2e/helpers/portable-evidence\.ts|frontend/playwright\.portable\.config\.ts|scripts/evals/browser-report\.mjs|scripts/diagnostics/browser-eval-report\.test\.mjs)$'; then
    (
        cd frontend
        EVAL_BROWSER=chromium npx playwright test --config playwright.portable.config.ts
    )
    EVAL_BROWSER=chromium node scripts/evals/browser-report.mjs frontend/eval-results/portable/chromium/core/results.json
fi

if [ "$(git rev-parse HEAD)" != "$candidate" ] || [ -n "$(git status --porcelain)" ]; then
    echo "The candidate changed during local checks. Verify the new revision before platform execution." >&2
    exit 1
fi

# Catch local static, coverage and scenario failures before remote builds.
# Both platforms still verify the same committed source before publication.
node scripts/pr-platform/run.mjs --base "$base_ref" "$@"

echo "PR preflight passed for $(git rev-parse --short HEAD). Run ./scripts/check-health.sh for the final full gate."
