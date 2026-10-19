#!/bin/bash

# Unified health check script for Mesh-Talk project
# This script runs all code quality checks and validations

set -e  # Exit on any error

stage="$(pwd)/scripts/validation/stage.mjs"

# FAST mode (FAST=1, or `check-health.sh --fast`): an optional partial local gate with
# formatting, clippy, frontend lint/tests, and the in-memory Rust
# unit slice. It SKIPS the slow, redundant steps (full `cargo test --workspace`, the verbose
# workspace build, the frontend build, and the supply-chain/secret/typos scans), because CI
# (.github/workflows/ci.yml) runs the full `cargo test --workspace` on all three platforms plus
# every one of those scans. Default (no flag) keeps the complete suite for CI/manual use.
FAST="${FAST:-0}"
case "${1:-}" in
    --fast) FAST=1 ;;
esac

if [ "$FAST" = "1" ]; then
    echo "Running Mesh-Talk health checks (FAST pre-commit gate)..."
else
    echo "Running Mesh-Talk health checks..."
fi

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    case $1 in
        "success")
            echo -e "${GREEN}✓${NC} $2"
            ;;
        "warning")
            echo -e "${YELLOW}⚠${NC} $2"
            ;;
        "error")
            echo -e "${RED}✗${NC} $2"
            ;;
        *)
            echo "$2"
            ;;
    esac
}

# Function to check if a command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Check if we're in the project root
if [ ! -f "Makefile" ] || [ ! -d "src-tauri" ] || [ ! -d "frontend" ]; then
    print_status "error" "Please run this script from the project root directory"
    exit 1
fi

# A full gate must not silently skip its scans and still claim everything passed.
if [ "$FAST" != "1" ]; then
    for tool in cargo-deny cargo-machete typos gitleaks shellcheck actionlint; do
        if ! command_exists "$tool"; then
            print_status "error" "Required full-health tool missing: $tool. Install it or use --fast for a partial local check."
            exit 1
        fi
    done
    print_status "success" "Validating GitHub Actions workflows (actionlint)..."
    actionlint
fi

print_status "success" "Checking repository hygiene..."
node scripts/check-repository-hygiene.mjs

# Check Rust toolchain
print_status "success" "Checking Rust toolchain..."
if ! command_exists cargo; then
    print_status "error" "Cargo not found. Please install Rust: https://www.rust-lang.org/"
    exit 1
fi

# Check code formatting (Rust)
print_status "success" "Checking Rust code formatting..."
# Run from the workspace root: `cargo fmt --all` invoked inside a member crate does NOT
# format the sibling crate, so it must run here to cover BOTH crates (mirrors CI).
if ! cargo fmt --all -- --check; then
    print_status "error" "Rust code formatting issues found. Run 'cargo fmt --all' to fix."
    exit 1
fi

# Check Node.js and npm
print_status "success" "Checking Node.js and npm..."
if ! command_exists node || ! command_exists npm; then
    print_status "error" "Node.js or npm not found. Please install Node.js: https://nodejs.org/"
    exit 1
fi

# Cargo resolves dependencies during Clippy, tests, and builds below; an eager
# `cargo build` here compiled the app twice. Install frontend dependencies only
# on a fresh checkout, and surface installation failures instead of hiding them.
if [ ! -f "frontend/node_modules/.package-lock.json" ] ||
   [ "frontend/package-lock.json" -nt "frontend/node_modules/.package-lock.json" ]; then
    print_status "success" "Installing frontend dependencies..."
    if ! { cd frontend && npm ci; }; then
        print_status "error" "Frontend dependency installation failed."
        exit 1
    fi
    cd ..
fi

# Check code formatting (Frontend)
print_status "success" "Checking frontend code formatting..."
if ! { cd frontend && node "$stage" frontend-format --profile shared --inputs frontend -- npx prettier --check src/; }; then
    print_status "error" "Frontend code formatting issues found. Run 'npx prettier --write src/' to fix."
    exit 1
fi
cd ..

# Run ESLint (Frontend linter). ESLint 9 uses flat config (eslint.config.js);
# the `--ext` flag was removed, so run the package script which lints the project.
print_status "success" "Running frontend linter (ESLint)..."
if ! { cd frontend && node "$stage" frontend-lint --profile shared --inputs frontend -- npm run lint; }; then
    print_status "error" "Frontend linting issues found. Please fix ESLint errors."
    exit 1
fi
cd ..

# Type-check application and E2E sources. Vite and Playwright transpilation alone
# do not establish that the scenario/helper TypeScript contracts are valid.
print_status "success" "Checking frontend and E2E TypeScript..."
if ! { cd frontend && node "$stage" frontend-types --profile shared --inputs frontend -- npm run typecheck; }; then
    print_status "error" "Frontend or E2E TypeScript errors found."
    exit 1
fi
cd ..

print_status "success" "Running delivery / AI eval smoke..."
if ! scripts/ai-eval-smoke.sh; then
    print_status "error" "Delivery / AI eval smoke failed. Fix the missing coverage or trigger wiring."
    exit 1
fi

print_status "success" "Running AI eval harness and diagnostic regression tests..."
if ! node --test scripts/test-ai-eval.mjs; then
    print_status "error" "AI eval harness tests failed."
    exit 1
fi

if ! node "$stage" diagnostics --profile shared -- node --test scripts/diagnostics/*.test.mjs; then
    print_status "error" "Diagnostic regression tests failed."
    exit 1
fi

print_status "success" "Checking target cache cleanup safeguards..."
python3 -m unittest discover -s scripts -p test_build_retention.py -v

# Keep Cargo's target/ from growing without bound. This is intentionally best-effort:
# cleanup failures should not block formatting, tests, or commits.
if [ -f "scripts/build-retention.py" ]; then
    print_status "success" "Pruning stale Cargo target cache when needed..."
    if ! python3 scripts/build-retention.py clean; then
        print_status "warning" "Cargo target cache cleanup failed; continuing."
    fi
fi

print_status "success" "Checking public help documents and read-only MCP..."
if ! npm ci --prefix tools/help --ignore-scripts || ! node --test tools/help/*.test.mjs; then
    print_status "error" "Public help validation failed."
    exit 1
fi

# Tauri's `generate_context!` validates that `frontendDist` exists even during clippy/test
# compilation. FAST mode intentionally skips the real frontend build, so create the ignored
# directory when needed; the full gate and CI still build real assets.
if [ "$FAST" = "1" ] && [ ! -d "frontend/dist" ]; then
    print_status "success" "Creating frontend/dist placeholder for Tauri context generation..."
    mkdir -p frontend/dist
fi

# A fresh checkout has no frontendDist. Build before Tauri's generate_context!
# runs in Clippy/tests; stale local dist must not hide this dependency.
if [ "$FAST" != "1" ]; then
    print_status "success" "Building the frontend before Tauri checks..."
    if ! { cd frontend && node "$stage" frontend-build --profile shared --inputs frontend --resource heavy --artifact dist -- npm run build; }; then
        print_status "error" "Frontend build failed."
        exit 1
    fi
    cd ..
fi

# Run Clippy (Rust linter). MUST mirror CI: --all-targets covers tests/benches/bins'
# test modules — without it, lints in test code only surface on CI (and break it).
print_status "success" "Running Rust linter (Clippy)..."
if ! { cd src-tauri && node "$stage" rust-clippy --profile shared --resource heavy -- cargo clippy --workspace --all-targets -- -D warnings; }; then
    print_status "error" "Rust linting issues found. Please fix Clippy warnings."
    exit 1
fi
cd ..

# Supply-chain / secret / spelling / shell scans below mirror CI and are slow or require extra
# tooling — skip them in FAST mode; CI (.github/workflows/ci.yml) runs them on every push.
if [ "$FAST" != "1" ]; then

# Spelling (typos) — mirrors CI's crate-ci/typos. Warn-skip if not installed.
print_status "success" "Checking spelling (typos)..."
if command_exists typos; then
    if ! typos; then
        print_status "error" "Spelling issues found (see _typos.toml to allowlist domain terms)."
        exit 1
    fi
else
    print_status "warning" "typos not installed (cargo install typos-cli) — skipping; CI will still check."
fi

# Supply-chain policy (cargo-deny) — mirrors CI. Warn-skip if not installed.
print_status "success" "Checking supply-chain policy (cargo-deny)..."
if command_exists cargo-deny; then
    if ! { cd src-tauri && cargo deny check; }; then
        print_status "error" "cargo-deny policy violation (advisories/bans/licenses/sources)."
        exit 1
    fi
    cd ..
else
    print_status "warning" "cargo-deny not installed — skipping; CI will still check."
fi

# Unused dependencies (cargo-machete) — mirrors CI. Warn-skip if not installed.
print_status "success" "Checking for unused dependencies (cargo-machete)..."
if command_exists cargo-machete; then
    # Run from the workspace root so BOTH crates (core + app) are scanned.
    if ! cargo machete; then
        print_status "error" "cargo-machete found unused dependencies."
        exit 1
    fi
else
    print_status "warning" "cargo-machete not installed — skipping; CI will still check."
fi

# Secret scan (gitleaks) — mirrors CI. Warn-skip if not installed.
print_status "success" "Scanning for secrets (gitleaks)..."
if command_exists gitleaks; then
    if ! gitleaks git . --config .gitleaks.toml --redact -v >/dev/null 2>&1; then
        print_status "error" "gitleaks found a potential secret (see .gitleaks.toml to allowlist)."
        exit 1
    fi
else
    print_status "warning" "gitleaks not installed — skipping; CI will still check."
fi

# Shellcheck scripts — mirrors CI.
print_status "success" "Linting shell scripts (shellcheck)..."
if command_exists shellcheck; then
    if ! shellcheck --severity=warning scripts/*.sh scripts/release/*.sh .claude/hooks/*.sh hooks/* 2>/dev/null; then
        print_status "error" "shellcheck found warnings."
        exit 1
    fi
else
    print_status "warning" "shellcheck not installed — skipping; CI will still check."
fi

fi  # end FAST-skip of scan block

# Run frontend unit tests (Vitest); full health also enforces the store coverage floor.
print_status "success" "Running frontend tests..."
if [ "$FAST" = "1" ]; then
    FRONTEND_TEST_SCRIPT="test"
    FRONTEND_TEST_STAGE="frontend-tests"
else
    FRONTEND_TEST_SCRIPT="test:coverage"
    FRONTEND_TEST_STAGE="frontend-coverage"
fi
if [ "$FAST" = "1" ]; then
    frontend_test_args=()
else
    frontend_test_args=(--artifact coverage/lcov.info)
fi
if ! { cd frontend && node "$stage" "$FRONTEND_TEST_STAGE" --profile shared --inputs frontend --resource heavy "${frontend_test_args[@]}" -- npm run "$FRONTEND_TEST_SCRIPT"; }; then
    print_status "error" "Frontend tests or coverage failed. Please fix the failing gate."
    exit 1
fi
cd ..

# Playwright UI E2E — drives the real React app (headless Chromium, mocked Tauri IPC).
# Vitest alone can't catch render/wiring regressions (a stale mock once crashed login;
# a CSS overflow once hid messages) — only the browser e2e does. In FAST (pre-commit)
# mode we run it ONLY when frontend files are staged, to keep non-frontend commits quick;
# the full `make check` and CI (e2e-ui.yml) always run it.
RUN_UI_E2E=1
if [ "$FAST" = "1" ]; then
    RUN_UI_E2E=0
    if git diff --cached --name-only -- frontend/ 2>/dev/null | grep -q .; then
        RUN_UI_E2E=1
    fi
fi
if [ "$RUN_UI_E2E" = "1" ]; then
    print_status "success" "Running UI E2E (Playwright)..."
    # All scenarios here are headless. Avoid downloading the unused headed
    # browser, and never hide setup failures behind a successful shell result.
    if ! ( cd frontend && npx playwright install --only-shell chromium ); then
        print_status "error" "Chromium runtime installation failed."
        exit 1
    fi
    # Match CI's bounded worker budget; keep failures visible without retries.
    # Parallel local browsers can exhaust a scenario's total time budget while
    # competing for rendering resources, rather than exposing a UI regression.
    if ! { cd frontend && node "$stage" browser-full --profile shared --resource heavy --artifact @../target/validation/browser-full.json -- node ../scripts/validation/browser.mjs full; }; then
        print_status "error" "UI E2E failed. Please fix the browser e2e regressions."
        exit 1
    fi
    cd ..
else
    print_status "success" "Skipping UI E2E (no staged frontend changes)."
fi

# Run tests.
# FAST: only the in-memory unit slice (lib tests of both crates). With the test-only cheap-KDF
# params (cfg(test) in mesh-talk-core), these run in well under a second instead of ~20 min.
# The full `cargo test --workspace` (heavy/integration/e2e) is left to CI.
if [ "$FAST" = "1" ]; then
    print_status "success" "Running fast Rust unit tests (lib only)..."
    if ! { cd src-tauri && node "$stage" core-unit-tests --profile shared --resource heavy -- cargo test -p mesh-talk-core --lib && node "$stage" host-unit-tests --profile shared --resource heavy -- cargo test -p mesh-talk --lib; }; then
        print_status "error" "Rust unit tests failed. Please fix test issues."
        exit 1
    fi
    cd ..
else
    print_status "success" "Running tests..."
    if ! node "$stage" workspace-tests --profile shared --resource heavy -- cargo test --workspace; then
        print_status "error" "Tests failed. Please fix test issues."
        exit 1
    fi
fi

# Rust security advisories are gated by cargo-deny above (its `advisories` check uses the same
# RUSTSEC database as cargo-audit, is BLOCKING, and is exactly what CI runs — allowlist a
# known/unfixable advisory in deny.toml). No separate cargo-audit step: it duplicated cargo-deny
# and a workspace tripped it (Cargo.lock at the repo root, not src-tauri), and an out-of-date
# local cargo-audit chokes parsing newer advisory entries.

# The npm vuln scan and the full release-style builds are slow and redundant for a pre-commit
# gate — skip them in FAST mode. CI builds the frontend + workspace on all three platforms and
# audits npm dependencies (dependency-review / audit-ci) on every push.
if [ "$FAST" != "1" ]; then

# Check for security vulnerabilities (Node.js)
print_status "success" "Checking for Node.js security vulnerabilities..."
if ! command_exists npx; then
    print_status "error" "npx not found. Please install Node.js: https://nodejs.org/"
    exit 1
fi

# BLOCKING: audit-ci fails on high/critical npm advisories (threshold + allowlist in
# frontend/audit-ci.json), so a vulnerable dependency can't be introduced silently. Fix the
# dependency or allowlist a known/unfixable advisory in audit-ci.json before committing.
if ! { cd frontend && npx --yes audit-ci --config audit-ci.json; }; then
    print_status "error" "Security vulnerabilities (high/critical) found in Node.js dependencies. Fix them or allowlist a known advisory in frontend/audit-ci.json."
    exit 1
fi
cd ..

# The documentation MCP has its own pinned dependency graph.
if ! npm audit --prefix tools/help --audit-level=high; then
    print_status "error" "High/critical vulnerabilities found in public help dependencies."
    exit 1
fi

# Build the project
print_status "success" "Building the project..."
if ! { cd src-tauri && node "$stage" workspace-build --profile shared --resource heavy -- cargo build --workspace --verbose; }; then
    print_status "error" "Build failed. Please fix build issues."
    exit 1
fi
cd ..

fi  # end FAST-skip of npm-audit + build steps

if [ "$FAST" = "1" ]; then
    print_status "success" "Fast pre-commit gate passed! (full suite + scans run in CI)"
else
    print_status "success" "All health checks passed!"
fi
exit 0
