# AI Eval Case Registry

These are stable, repeatable cases for both the deterministic AI regression smoke gate and
the real model-backed AI eval. The canonical executable suite is
`docs/evals/ai-eval-suite.json`.

| Case | Stable example | Expected guard |
|------|----------------|----------------|
| MT-AI-001 | `.claude/agents/code-reviewer.md` | Reviews lead with correctness/security/test findings and concrete file references. |
| MT-AI-002 | `.claude/agents/e2e-runner.md` | Runs `make e2e`, triages startup timeout vs environment vs real delivery regression, and never claims green without observed output. |
| MT-AI-003 | `.claude/agents/bug-hunter.md` | Reports only verified bugs with repro, severity, blast radius, and fix direction. |
| MT-AI-004 | `.claude/settings.json` | Claude Code edits to key files trigger the AI eval smoke gate, and heavy cargo commands stay under the CPU guard. |
| MT-AI-005 | `.github/pull_request_template.md` | PRs ask authors to document AI eval / smoke commands when code, prompts, models, agent rules, or core flows change. |

Run:

```bash
make eval-smoke
AI_EVAL_COMMAND='<non-interactive model command>' make ai-eval
```

## Core Smoke Examples

| Case | Stable example | Expected guard |
|------|----------------|----------------|
| MT-SMOKE-001 | `crates/mesh-talk-core/tests/decoder_smoke.rs` | Untrusted wire decoder smoke stays in the normal Rust test suite. |
| MT-SMOKE-002 | `make e2e` | Real `mesh-talk-node` two-node, persistent-history, post-office, channel, and file flows remain runnable. |
| MT-SMOKE-003 | `frontend/e2e/chat.spec.ts` and `message-lifecycle.spec.ts` | Browser-level chat lifecycle regressions remain covered. |
| MT-SMOKE-004 | `frontend/e2e/ui-visual-regression.spec.ts` and UI audit specs | Theme, layout, and interaction regressions keep stable Playwright coverage. |
