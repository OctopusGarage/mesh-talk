#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ "${1:-}" == "--changed" ]]; then
  changed="${2:-}"
  [[ -n "$changed" ]] || exit 0
  case "$changed" in
    "$ROOT_DIR"/*) changed="${changed#"$ROOT_DIR"/}" ;;
  esac
  case "$changed" in
    AGENTS.md | Makefile | hooks/* | scripts/* | .claude/* | \
      .github/* | docs/evals/* | docs/ARCHITECTURE.md | docs/README.md | \
      specifications/testing_guidelines.md | specifications/task_completion_criteria.md | \
      crates/mesh-talk-core/src/node/* | crates/mesh-talk-core/src/transport/* | \
      crates/mesh-talk-core/src/discovery/* | crates/mesh-talk-core/src/eventlog/* | \
      crates/mesh-talk-core/src/ratchet/* | crates/mesh-talk-core/src/channel/* | \
      crates/mesh-talk-core/src/file/* | crates/mesh-talk-core/src/postoffice/* | \
      crates/mesh-talk-core/src/storage/* | crates/mesh-talk-core/src/dm.rs)
      ;;
    *)
      exit 0
      ;;
  esac
fi

failures=0

record_failure() {
  printf 'ai-eval-smoke: %s\n' "$1" >&2
  failures=$((failures + 1))
}

require_file() {
  local path="$1"
  [[ -f "$path" ]] || record_failure "missing required file: $path"
}

require_executable() {
  local path="$1"
  [[ -x "$path" ]] || record_failure "required script is not executable: $path"
}

require_contains() {
  local path="$1"
  local needle="$2"
  local label="$3"
  if [[ ! -f "$path" ]]; then
    record_failure "cannot check $label; missing file: $path"
    return
  fi
  grep -Fq -- "$needle" "$path" || record_failure "$label missing in $path: $needle"
}

require_regex() {
  local path="$1"
  local pattern="$2"
  local label="$3"
  if [[ ! -f "$path" ]]; then
    record_failure "cannot check $label; missing file: $path"
    return
  fi
  grep -Eq "$pattern" "$path" || record_failure "$label missing in $path: $pattern"
}

# Stable docs and examples for delivery / AI regression checks.
require_file "docs/evals/delivery-ai-regression.md"
require_file "docs/evals/ai-eval-cases.md"
require_file "docs/evals/ai-eval-suite.json"
require_file "docs/evals/module-coverage.json"
require_file "docs/evals/prompts/agent-contract-evaluator.md"
require_contains "docs/evals/ai-eval-cases.md" "MT-AI-001" "AI eval case registry"
require_contains "docs/evals/delivery-ai-regression.md" "make eval-smoke" "delivery eval runbook"
require_contains "docs/evals/delivery-ai-regression.md" "make ai-eval" "real AI eval runbook"
require_contains "docs/evals/delivery-ai-regression.md" "make smoke-full" "full real smoke runbook"
require_contains "docs/README.md" "delivery-ai-regression.md" "documentation map entry"

# Core feature smoke and regression examples.
require_file "crates/mesh-talk-core/tests/decoder_smoke.rs"
require_file "crates/mesh-talk-core/tests/two_node_cli.rs"
require_file "crates/mesh-talk-core/tests/persistent_history.rs"
require_file "crates/mesh-talk-core/tests/post_office_offline.rs"
require_file "crates/mesh-talk-core/tests/channel_and_file_cli.rs"
require_contains "Makefile" "cargo test --workspace" "workspace test entry"
require_contains "Makefile" "make e2e" "backend e2e help entry"
require_contains "Makefile" "--test two_node_cli --test persistent_history --test post_office_offline --test channel_and_file_cli" "backend e2e suite wiring"

# Frontend and UI workflow smoke coverage.
require_file "frontend/e2e/chat.spec.ts"
require_file "frontend/e2e/message-lifecycle.spec.ts"
require_file "frontend/e2e/ui-visual-regression.spec.ts"
require_file "frontend/e2e/ui-audit-shell.spec.ts"
require_file "frontend/e2e/ui-audit-message-log.spec.ts"
require_file "frontend/e2e/ui-audit-composer.spec.ts"
require_file "frontend/e2e/ui-audit-dialogs.spec.ts"
require_contains "frontend/package.json" "\"e2e\": \"playwright test\"" "frontend e2e npm entry"
require_contains ".github/workflows/e2e-ui.yml" "npx playwright test" "UI e2e CI entry"

# AI / agent rule contracts. These are deterministic prompt-contract checks, not LLM scoring.
require_file "AGENTS.md"
require_file ".claude/agents/code-reviewer.md"
require_file ".claude/agents/e2e-runner.md"
require_file ".claude/agents/bug-hunter.md"
require_file ".claude/commands/audit.md"
require_file ".claude/commands/e2e.md"
require_contains ".claude/agents/code-reviewer.md" "Correctness" "code-reviewer review rubric"
require_contains ".claude/agents/code-reviewer.md" "Security" "code-reviewer security rubric"
require_contains ".claude/agents/code-reviewer.md" "Tests" "code-reviewer test rubric"
require_contains ".claude/agents/e2e-runner.md" "make e2e" "e2e-runner command contract"
require_contains ".claude/agents/e2e-runner.md" "Never claim green" "e2e-runner verification contract"
require_contains ".claude/agents/bug-hunter.md" "verified" "bug-hunter evidence contract"
require_contains ".claude/settings.json" "cargo-nice-guard.sh" "Claude cargo guard hook"
require_contains ".claude/settings.json" "ai-eval-smoke.sh" "Claude AI eval hook"

# Explicit local and CI triggers.
require_contains "Makefile" "eval-smoke:" "Makefile eval-smoke target"
require_contains "Makefile" "ai-eval:" "Makefile real AI eval target"
require_contains "Makefile" "smoke-full:" "Makefile full real smoke target"
require_executable "scripts/ai-eval-smoke.sh"
require_executable "scripts/ai-eval.mjs"
require_executable "scripts/run-real-smoke.sh"
require_contains "scripts/check-health.sh" "scripts/ai-eval-smoke.sh" "check-health eval trigger"
require_contains "scripts/check-health.sh" "scripts/test-ai-eval.mjs" "check-health AI eval harness tests"
require_contains "scripts/check-health.sh" "frontend/dist" "fast gate Tauri frontendDist guard"
require_contains "hooks/pre-commit" "./scripts/check-health.sh --fast" "pre-commit fast gate trigger"
require_contains ".github/workflows/ci.yml" "make eval-smoke" "CI eval-smoke trigger"
require_contains ".github/workflows/ai-eval.yml" "make ai-eval" "CI/manual real AI eval trigger"
require_contains ".github/pull_request_template.md" "AI eval / smoke" "PR validation checklist"
require_contains "specifications/task_completion_criteria.md" "make eval-smoke" "definition-of-done eval entry"
require_regex "specifications/testing_guidelines.md" "AI eval|eval-smoke" "testing guideline eval entry"

if [[ "$failures" -gt 0 ]]; then
  printf 'ai-eval-smoke: failed with %s issue(s).\n' "$failures" >&2
  exit 1
fi

printf 'ai-eval-smoke: delivery, smoke, and AI rule contracts are wired.\n'
