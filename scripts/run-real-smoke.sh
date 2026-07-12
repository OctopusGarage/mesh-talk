#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

run_step() {
  local name="$1"
  shift
  echo "==> $name"
  "$@"
}

run_step "delivery / AI contract smoke" make eval-smoke
run_step "AI eval harness tests" node --test scripts/test-ai-eval.mjs
run_step "workspace Rust tests" make test
run_step "backend multi-process E2E" make e2e
run_step "frontend Playwright E2E" bash -lc 'cd frontend && npm run e2e'

echo "real smoke suite completed"
