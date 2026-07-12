# Delivery And AI Regression Gate

## Coverage

- Core protocol smoke: workspace Rust tests plus stable examples for decoder, two-node DM,
  persistent history, post-office offline delivery, channel, and file CLI flows.
- Frontend smoke: Vitest unit tests, Playwright chat lifecycle tests, UI audit tests, and
  visual regression snapshots.
- AI / agent contracts: deterministic checks plus a real model-backed eval suite for Claude
  agents, slash commands, repo agent guidance, and this runbook.
- Delivery wiring: `Makefile`, `check-health`, pre-commit, CI, and PR template keep explicit
  validation entry points.

## Run Commands

```bash
make eval-smoke
make ai-eval
make smoke-full
./scripts/check-health.sh --fast
./scripts/check-health.sh
make test
make e2e
cd frontend && npm run e2e
```

`make ai-eval` requires a real model provider:

```bash
AI_EVAL_COMMAND='claude -p --output-format text --permission-mode dontAsk' make ai-eval
AI_EVAL_COMMAND='codex exec --sandbox read-only --ask-for-approval never' make ai-eval
```

The command receives one eval prompt on stdin and must print the JSON verdict requested by
`docs/evals/prompts/agent-contract-evaluator.md`.

## Judgment Standard

- `make eval-smoke` must pass for changes to code, prompts, model configuration, agent
  rules, workflow scripts, CI, or core protocol flows.
- `make ai-eval` must pass for prompt, model, agent rule, eval suite, or core workflow
  changes that affect AI-assisted behavior. If a model provider is unavailable locally, run
  the `AI Eval` workflow with `AI_EVAL_COMMAND` configured and record the run URL.
- `make smoke-full` is the complete real smoke command for release-critical changes. It
  runs deterministic eval contracts, AI eval harness tests, workspace Rust tests, backend
  multi-process E2E, and frontend Playwright E2E.
- `./scripts/check-health.sh --fast` must pass before commit.
- `./scripts/check-health.sh` must pass before review or release.
- Backend networking / relay / sync / crypto changes must run `make e2e` or document why
  the environment cannot run multicast/TCP tests.
- UI behavior or layout changes must run the relevant Playwright spec, and visual snapshot
  updates require human review of the diff images.

## Trigger Rules

- Local pre-commit runs `./scripts/check-health.sh --fast`, which includes `make eval-smoke`.
- CI runs `make eval-smoke` and the AI eval harness tests directly on Ubuntu and still runs
  the normal test, lint, security, backend E2E, and UI E2E workflows.
- `.github/workflows/ai-eval.yml` runs deterministic eval checks on eval/prompt/agent PRs.
  Its real model-backed job runs `make ai-eval` when the repository secret
  `AI_EVAL_COMMAND` is configured; otherwise it emits a warning so the missing provider is
  visible.
- Claude Code runs `scripts/ai-eval-smoke.sh --changed <file>` after edits to key files.
  Non-critical files are ignored; critical prompt, workflow, script, and core-flow files run
  the smoke gate immediately.
- Codex and other agents are covered through the shared git hooks, `AGENTS.md`, PR template,
  and CI gate.

## Known Gaps

- `make eval-smoke` is deterministic; `make ai-eval` is the real model-backed eval.
- `make ai-eval` cannot prove anything without `AI_EVAL_COMMAND`; absence of that provider
  is a failed local precondition, not a pass.
- `make smoke-full` is intentionally slow because it runs real backend and frontend E2E.
- Manual review is still required for UX judgment, visual snapshot diffs, release notes,
  security tradeoffs, and any model/provider behavior changes.

## Hardening Backlog

- Capture anonymized real failures as new `MT-AI-*` or `MT-SMOKE-*` examples when a
  regression escapes the current gate.
- Make branch protection require the dedicated CI `make eval-smoke` step if GitHub settings
  allow a stable per-job required check.
