# AI Eval Case Registry

These are stable, repeatable cases for both the deterministic AI regression smoke gate and
the real model-backed AI eval. The canonical executable suite is
`docs/evals/ai-eval-suite.json`.

| Case | Stable example | Expected guard |
|------|----------------|----------------|
| MT-AI-001 | `.claude/agents/code-reviewer.md` | Reviews lead with correctness/security/test findings and concrete file references. |
| MT-AI-002 | `.claude/agents/e2e-runner.md` | Runs `make e2e`, triages startup timeout vs environment vs real delivery regression, and never claims green without observed output. |
| MT-AI-003 | `.claude/agents/bug-hunter.md` | Reports only verified bugs with repro, severity, blast radius, and fix direction. |
| MT-AI-004 | `AGENTS.md` | Project guidance preserves module boundaries, validation commands, and security expectations. |
| MT-AI-005 | `docs/evals/delivery-ai-regression.md` | Delivery guidance distinguishes deterministic and model-backed evaluation, trigger rules, and human review. |
| MT-AI-006 | `docs/post-office-metadata-privacy.md` | Relay privacy claims match visible routing metadata, custody limits, key compromise, and V1 migration behavior. |
| MT-AI-007 | `docs/post-office-macos-service.md` | Intel Mac service setup, credential handling, health checks, lifecycle limits, and separate cross-machine offer and file-byte relay verification. |
| MT-AI-008 | `docs/post-office-windows-service.md` | Windows Scheduled Task setup, credential and lock handling, health checks, and separate cross-machine delivery layers. |
| MT-AI-009 | `docs/routed-network-discovery.md` | Tailscale candidate identity boundary, opt-in A/B/C/E bridge flow, opaque message delivery, and explicit network limits. |
| MT-AI-010 | `docs/routed-network-discovery.md` | Two pinned post-office gateways share one elected directory and mailbox, with cross-LAN E2E evidence and no replication claim. |

Run:

```bash
make eval-smoke
AI_EVAL_COMMAND='<non-interactive model command>' make ai-eval
AI_EVAL_COMMAND='<non-interactive model command>' node scripts/ai-eval.mjs --suite docs/evals/linked-device-sync-ai-eval-suite.json --report target/ai-eval/linked-device-sync-report.json
```

The linked-device suite reviews source against a security rubric. It supplements the Rust unit tests and real-process E2E runs; model scores alone do not verify runtime behavior.

The [tray menu suite](tray-menu-ai-eval-suite.json) reviews native menu actions,
frontend navigation, and the controlled About dialog. Its
[validation guide](tray-menu.md) records the regression coverage and native-test limits.

```bash
AI_EVAL_COMMAND='<non-interactive model command>' node scripts/ai-eval.mjs --suite docs/evals/tray-menu-ai-eval-suite.json --report target/ai-eval/tray-menu-report.json
```

## Core Smoke Examples

| Case | Stable example | Expected guard |
|------|----------------|----------------|
| MT-SMOKE-001 | `crates/mesh-talk-core/tests/decoder_smoke.rs` | Untrusted wire decoder smoke stays in the normal Rust test suite. |
| MT-SMOKE-002 | `make e2e` | Real `mesh-talk-node` two-node, persistent-history, post-office, channel, and file flows remain runnable. |
| MT-SMOKE-003 | `frontend/e2e/chat.spec.ts` and `message-lifecycle.spec.ts` | Browser-level chat lifecycle regressions remain covered. |
| MT-SMOKE-004 | `frontend/e2e/ui-visual-regression.spec.ts` and UI audit specs | Theme, layout, and interaction regressions keep stable Playwright coverage. |
