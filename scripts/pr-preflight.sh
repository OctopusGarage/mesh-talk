#!/usr/bin/env bash
# Fast, change-focused feedback before opening a PR. Pass relevant Playwright specs
# when frontend rendering changed; the coverage report names lines still untested.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [ -n "$(git status --porcelain)" ]; then
    echo "Commit or stash changes before PR preflight so the diff matches the tested revision." >&2
    exit 1
fi

base_ref="${PATCH_COVERAGE_BASE:-origin/main}"
git rev-parse --verify "$base_ref" >/dev/null
merge_base="$(git merge-base HEAD "$base_ref")"
changed_files="$(git diff --name-only "$merge_base" HEAD)"

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
    if [ "$#" -gt 0 ]; then
        # JavaScript template literals belong to Node.
        # shellcheck disable=SC2016
        node --input-type=module -e 'import { readdir, unlink } from "node:fs/promises"; const dir="frontend/coverage/e2e-parts"; for (const name of await readdir(dir).catch(() => [])) if (name.endsWith(".json")) await unlink(`${dir}/${name}`);'
        (
            cd frontend
            MESH_TALK_E2E_COVERAGE=1 npx playwright test --project=chromium --workers=1 --retries=0 "$@"
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

echo "PR preflight passed for $(git rev-parse --short HEAD). Run ./scripts/check-health.sh for the final full gate."
