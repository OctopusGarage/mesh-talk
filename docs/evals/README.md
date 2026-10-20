# Evaluation suites and case registry

This directory contains maintained evaluation inputs, not generated reports or
historical implementation reviews. Keep output under ignored `target/ai-eval/`
or `tmp/`; put current guidance in its owning document.

These are stable, repeatable cases for both the deterministic AI regression smoke gate and
the real model-backed AI eval. The canonical executable suite is
`docs/evals/ai-eval-suite.json`.

| Case      | Stable example                         | Expected guard                                                                                                                                                                                                                      |
| --------- | -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| MT-AI-001 | `.claude/agents/code-reviewer.md`      | Reviews lead with correctness/security/test findings and concrete file references.                                                                                                                                                  |
| MT-AI-002 | `.claude/agents/e2e-runner.md`         | Runs `make e2e`, triages startup timeout vs environment vs real delivery regression, and never claims green without observed output.                                                                                                |
| MT-AI-003 | `.claude/agents/bug-hunter.md`         | Reports only verified bugs with repro, severity, blast radius, and fix direction.                                                                                                                                                   |
| MT-AI-004 | `AGENTS.md`                            | Project guidance preserves module boundaries, validation commands, and security expectations.                                                                                                                                       |
| MT-AI-005 | `docs/evals/README.md`                 | Delivery guidance distinguishes deterministic and model-backed evaluation, trigger rules, and human review.                                                                                                                         |
| MT-AI-006 | `docs/post-office-metadata-privacy.md` | Relay privacy claims match visible routing metadata, custody limits, key compromise, and V1 migration behavior.                                                                                                                     |
| MT-AI-007 | `docs/post-office-macos-service.md`    | Intel Mac service setup, credential handling, health checks, lifecycle limits, and separate cross-machine offer and file-byte relay verification.                                                                                   |
| MT-AI-008 | `docs/post-office-windows-service.md`  | Windows Scheduled Task setup, credential and lock handling, health checks, and separate cross-machine delivery layers.                                                                                                              |
| MT-AI-009 | `docs/routed-network-discovery.md`     | Tailscale candidate identity boundary, opt-in A/B/C/E bridge flow, opaque message delivery, and explicit network limits.                                                                                                            |
| MT-AI-010 | `docs/routed-network-discovery.md`     | Equal enrolled post offices exchange eligible signed presence and recipient-encrypted mail with signed consent/completion; direct relay selection, onward routes, bounded reconciliation and custody/evidence limits stay explicit. |

Suite version 2 follows the [current communication baseline](../network-design-reference.md).
It distinguishes equal-relay federation from the older pinned TCP gateway. It
must not require obsolete direct sender/recipient overlap for federated consent,
prohibit implemented version-3 reconciliation, or mandate different ports on
separate hosts. Authentication, privacy, recipient confirmation, custody limits
and existing passing thresholds remain required.

Run:

```bash
make eval-smoke
AI_EVAL_COMMAND='<non-interactive model command>' make ai-eval
AI_EVAL_COMMAND='<non-interactive model command>' node scripts/ai-eval.mjs --suite docs/evals/linked-device-sync-ai-eval-suite.json --report target/ai-eval/linked-device-sync-report.json
```

Cases can declare `context_files` alongside their primary `subject_file`. The
runner includes each supporting source in the model prompt and records its path
in the report. This lets the model assess delegated production validation without
requiring duplicated guards in an orchestration module. Unprovided dependencies
remain unverified; minimum passing scores and security requirements still apply.
When ownership moves behind a helper, update the affected case contexts with the
implementation that proves its rubric guarantees. A facade or re-export alone
does not establish the delegated behavior; report missing context separately
from a confirmed product defect.
The linked-device suite version 2 includes the page assembler/importer and wire
helpers, requires accepted requester membership, and distinguishes MTAHS2 file
metadata selection from attachment bytes while preserving MTAHS1 behavior.

The linked-device suite reviews source against a security rubric. It supplements the Rust unit tests and real-process E2E runs; model scores alone do not verify runtime behavior.

The [tray menu suite](tray-menu-ai-eval-suite.json) reviews native menu actions,
frontend navigation, and the controlled About dialog. Use the tray Rust unit
tests, `scripts/diagnostics/tray-geometry-report.test.mjs`, and
`frontend/e2e/tray-navigation.spec.ts` for executable regressions. Browser mocks
and source judgments do not prove native OS menu interaction or focus behavior.

```bash
AI_EVAL_COMMAND='<non-interactive model command>' node scripts/ai-eval.mjs --suite docs/evals/tray-menu-ai-eval-suite.json --report target/ai-eval/tray-menu-report.json
```

## Group reception

The [group reception suite](group-reception-ai-eval-suite.json) reviews causal
ordering under clock skew, reception classification, notification privacy and
batching, late unread identities, stable reading anchors, and recovery knowledge
boundaries. Supply the production helper modules named by each case as supporting
context. These judgments supplement executable Rust and browser regressions;
they do not establish delivery or visual correctness by themselves.

```bash
AI_EVAL_COMMAND='<non-interactive model command>' node scripts/ai-eval.mjs --suite docs/evals/group-reception-ai-eval-suite.json --report target/ai-eval/group-reception-report.json
```

## Core Smoke Examples

| Case         | Stable example                                                 | Expected guard                                                                                            |
| ------------ | -------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| MT-SMOKE-001 | `crates/mesh-talk-core/tests/decoder_smoke.rs`                 | Untrusted wire decoder smoke stays in the normal Rust test suite.                                         |
| MT-SMOKE-002 | `make e2e`                                                     | Real `mesh-talk-node` two-node, persistent-history, post-office, channel, and file flows remain runnable. |
| MT-SMOKE-003 | `frontend/e2e/chat.spec.ts` and `message-lifecycle.spec.ts`    | Browser-level chat lifecycle regressions remain covered.                                                  |
| MT-SMOKE-004 | `frontend/e2e/ui-visual-regression.spec.ts` and UI audit specs | Theme, layout, and interaction regressions keep stable Playwright coverage.                               |

## Person presence

The [person presence suite](person-presence-ai-eval-suite.json) evaluates the
client-relative contract, account/device aggregation, frontend expiry and error
state machine, and original directory freshness. It complements the default
agent-workflow suite; model judgments do not replace unit or loopback evidence.

```bash
AI_EVAL_COMMAND='<non-interactive model command>' node scripts/ai-eval.mjs --suite docs/evals/person-presence-ai-eval-suite.json --report target/ai-eval/person-presence-report.json
```

## Evaluation workflow and limits

`make eval-smoke` checks deterministic repository contracts and required scenario
wiring without calling a model. `make ai-eval` runs the source cases above using
`AI_EVAL_COMMAND` or the configured HTTP provider. A missing provider is **not
run**, not a model-backed pass. The runner records per-case results and binds the
report to the supplied source context. Exit success alone does not establish
runtime messaging, native UI behavior or usability.

Run focused checks while changing a feature. Before PR/review, use the required
[testing gates](../testing-feedback-loop.md) and [quality gates](../quality-gates.md).
`make smoke-full` is the explicit complete backend/browser smoke entry point.
It includes deterministic contracts, harness tests, workspace tests, frontend
coverage, backend multi-process E2E and browser E2E. Do not invoke it for every
local edit or treat it as physical desktop acceptance.

Changes to agents, prompts, suites, gate scripts or their documented contracts
require `make eval-smoke` and `node --test scripts/test-ai-eval.mjs`. Run a real
model when the changed prompt/rubric needs behavioral evaluation, and disclose
provider unavailability. Preserve case thresholds and supplied security context;
never replace missing native or model evidence with source assertions.

UI review uses [the reusable screenshot prompt](prompts/ui-ux-review.md), actual
screenshots and interaction assertions. A screenshot alone cannot establish task
success. See [platform validation](../platform-validation.md) for native/browser
boundaries and [quality gates](../quality-gates.md) for evidence requirements.

### Triggers, full smoke and human review

- Local edits: run deterministic smoke after changing its contracts. Codex must
  invoke it explicitly; a chat instruction alone is not execution evidence.
  Claude Code's configured change hook invokes `scripts/ai-eval-smoke.sh --changed`
  for relevant paths; use `make eval-smoke` to check the complete contract.
- CI and PR: the AI workflow runs deterministic contracts and harness tests;
  real-model evaluation depends on an available configured provider. Inspect the
  job report instead of treating a skipped provider as success.
- Full backend smoke: `make test && make e2e`. Full frontend smoke:
  `cd frontend && npm test && npm run e2e`. Use these only at the required gate
  or on explicit request; ordinary development uses affected selections.
- Human review remains required for security/crypto authority changes, product
  usability, visual snapshot changes and native/platform acceptance. An AI score
  or static screenshot cannot replace that review. Keep missing physical-device,
  remote-network and model-provider evidence explicit in the PR.

The [validation velocity suite](validation-velocity-ai-eval-suite.json) evaluates
selection completeness, local reuse and cancellation, CI gate preservation,
feedback measurement limits, future requirement maintenance and patch-coverage
applicability
(`MT-VALIDATION-001` through `MT-VALIDATION-006`). Run
`node scripts/ai-eval.mjs --suite docs/evals/validation-velocity-ai-eval-suite.json`
with the model provider described above. Model review complements executable
regressions and hosted checks; it cannot certify timing improvements or replace
required platform/native evidence.
