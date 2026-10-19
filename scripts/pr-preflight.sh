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
const flags = parseFlags(['--base', process.env.PATCH_COVERAGE_BASE ?? 'origin/main', ...process.argv.slice(2)]);
if (flags.planOnly) throw new Error('Preview with node scripts/pr-platform/run.mjs --plan; PR preflight requires execution.');
console.log(flags.base);
JS
)"
git rev-parse --verify "$base_ref" >/dev/null
merge_base="$(git merge-base HEAD "$base_ref")"
changed_files="$(git diff --name-only --no-renames "$merge_base" HEAD)"

# One plan owns automatic scenarios and explicit selectors. Validate native proof
# before spending time on local coverage or platform builds.
scenario_plan="$(node scripts/pr-platform/run.mjs --plan --base "$base_ref" "$@")"
ui_specs=()
while IFS= read -r spec; do
    if [ -n "$spec" ]; then ui_specs+=("$spec"); fi
done < <(printf '%s' "$scenario_plan" | node --input-type=module -e 'let input=""; for await (const chunk of process.stdin) input+=chunk; console.log(JSON.parse(input).uiSpecs.join("\n"));')
node scripts/validation/pr-scenarios.mjs verify "$base_ref"

# Reuse local work only. Required environments are checked on every invocation.
if [ "${MESH_VALIDATION_PREFLIGHT:-0}" != 1 ]; then
    env MESH_VALIDATION_PREFLIGHT=1 node scripts/validation/stage.mjs local-preflight \
        --context "$base_ref:$(git rev-parse "$base_ref")" \
        --artifact @target/validation/preflight-artifacts.json --resource preflight \
        -- bash "$repo_root/scripts/pr-preflight.sh" "$@"
    if [ "$(git rev-parse HEAD)" != "$candidate" ] || [ -n "$(git status --porcelain)" ]; then
        echo "The candidate changed before platform execution." >&2
        exit 1
    fi
    platform_resource=platforms
    if [ -z "${MESH_PR_LINUX_HOST:-}" ]; then platform_resource=heavy; fi
    MESH_VALIDATION_FRESH=1 node scripts/validation/stage.mjs required-platforms \
        --resource "$platform_resource" -- node scripts/pr-platform/run.mjs --base "$base_ref" "$@"
    echo "PR preflight passed for $(git rev-parse --short HEAD). Run ./scripts/check-health.sh for the final full gate."
    exit 0
fi
stage="$repo_root/scripts/validation/stage.mjs"

typos
cargo fmt --all -- --check
if printf '%s\n' "$changed_files" | rg -q '^frontend/'; then
    (
        cd frontend
        node "$stage" frontend-format --profile shared --inputs frontend -- npx prettier --check src/
        node "$stage" frontend-types --profile shared --inputs frontend -- npm run typecheck
        node "$stage" frontend-lint --profile shared --inputs frontend -- npm run lint
    )
fi
node "$stage" diagnostics --profile shared -- node --test scripts/diagnostics/*.test.mjs

if printf '%s\n' "$changed_files" | node scripts/pr-platform/rust-packages.mjs --selection-needed; then
    # Tauri validates frontendDist even when listing its test executable.
    (
        cd frontend
        node "$stage" frontend-build --profile shared --inputs frontend --resource heavy --artifact dist -- npm run build
    )
    node "$stage" rust-selectors --profile shared --resource heavy --artifact target/validation/selector-inventory.json \
        -- node scripts/validation/selectors.mjs "$@"
fi

if printf '%s\n' "$changed_files" | rg -q '^frontend/'; then
    (
        cd frontend
        node "$stage" frontend-coverage --profile shared --inputs frontend --resource heavy --artifact coverage/lcov.info -- npm run test:coverage
    )

    coverage_reports=(frontend/coverage/lcov.info)
    if [ "${#ui_specs[@]}" -gt 0 ]; then
        (
            cd frontend
            node "$stage" browser-coverage --profile shared --resource heavy \
                --artifact coverage/e2e.lcov --artifact @../target/validation/browser-coverage.json \
                -- node ../scripts/validation/browser.mjs coverage "${ui_specs[@]}"
        )
        coverage_reports+=(frontend/coverage/e2e.lcov)
    fi
    PATCH_COVERAGE_BASE="$base_ref" node scripts/patch-coverage.mjs "${coverage_reports[@]}"
fi

if printf '%s\n' "$changed_files" | rg -q '^(frontend/e2e/portable-core\.spec\.ts|frontend/e2e/helpers/portable-evidence\.ts|frontend/playwright\.portable\.config\.ts|scripts/evals/browser-report\.mjs|scripts/diagnostics/browser-eval-report\.test\.mjs)$'; then
    (
        cd frontend
        EVAL_BROWSER=chromium node "$stage" browser-portable --profile shared --resource heavy \
            --artifact @../target/validation/browser-portable.json \
            -- node ../scripts/validation/browser.mjs portable
    )
fi

if [ "$(git rev-parse HEAD)" != "$candidate" ] || [ -n "$(git status --porcelain)" ]; then
    echo "The candidate changed during local checks. Verify the new revision before platform execution." >&2
    exit 1
fi

# The outer invocation always executes required platform checks after this stage.
node --input-type=module <<'JS'
import { readFile, writeFile, access } from 'node:fs/promises';
const artifacts = [];
for (const filename of ['frontend/coverage/lcov.info', 'frontend/coverage/e2e.lcov']) {
  try { await access(filename); artifacts.push(filename); } catch { /* Not selected by this plan. */ }
}
for (const mode of ['coverage', 'portable']) {
  try { artifacts.push(...JSON.parse(await readFile(`target/validation/browser-${mode}.json`, 'utf8'))); } catch { /* Not selected. */ }
}
await writeFile('target/validation/preflight-artifacts.json', JSON.stringify(artifacts, null, 2));
JS
