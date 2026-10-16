# Documentation

This is the navigation hub for Mesh-Talk's current guides, technical references, and
dated evidence. Start with the path that matches your task. The
[documentation guidelines](DOCUMENTATION_GUIDELINES.md) explain where new information
belongs and how to keep the site, README, and guides aligned.

| I want to…                         | Start here                                          | Continue with                                                                                                                             |
| ---------------------------------- | --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| Install or try the desktop app     | [Project overview](../README.md)                    | [Install and update](updates.md) · [Troubleshooting](../specifications/troubleshooting_guide.md)                                          |
| Connect peers across networks      | [Network topologies](network-topologies.md)         | [Scenario Q&A](network-design-qa.md) · [Routed discovery](routed-network-discovery.md) · [Relay privacy](post-office-metadata-privacy.md) |
| Run a headless node or relay       | [Deployment](../specifications/deployment_guide.md) | [Relay releases](post-office-release.md)                                                                                                  |
| Understand or change the protocol  | [Architecture](ARCHITECTURE.md)                     | [Domain model](../CONTEXT.md) · [Testing feedback loop](testing-feedback-loop.md)                                                         |
| Contribute to the desktop app      | [Contributing](../CONTRIBUTING.md)                  | [Visual system](../frontend/DESIGN.md) · [Customization packs](CUSTOMIZATION_PACKS.md)                                                    |
| Review security or past validation | [Security policy](../SECURITY.md)                   | [Architecture security posture](ARCHITECTURE.md#8-security-posture-summary) · [Evaluation records](#evaluation-records)                   |

## Current product and architecture

| Doc                                                                                 | What it covers                                                                                                                            |
| ----------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| [`README.md`](../README.md)                                                         | Project overview, install, quick start.                                                                                                   |
| [`PROJECT.md`](../PROJECT.md)                                                       | Top-level context pointer for AI tools / new contributors.                                                                                |
| [`CONTEXT.md`](../CONTEXT.md)                                                       | Domain model — entities, layers, invariants, what the project does _not_ own.                                                             |
| [`docs/ARCHITECTURE.md`](ARCHITECTURE.md)                                           | Authoritative technical reference — crypto, event log + sync, transport, discovery, post office, build/test/CI.                           |
| [`docs/routed-network-discovery.md`](routed-network-discovery.md)                   | Optional routed discovery over existing Tailscale or manually supplied addresses, identity boundary, route selection, and release checks. |
| [`docs/routed-network-discovery-research.md`](routed-network-discovery-research.md) | Historical cited research and design options behind routed discovery; see the implementation guide for current behavior.                  |
| [`docs/network-topologies.md`](network-topologies.md)                               | Review baseline: network/data planes, switch matrix, LAN/Tailscale/federation diagrams, synchronization paths, and evidence limits.       |
| [`docs/network-design-qa.md`](network-design-qa.md)                                 | Scenario-by-scenario questions, current answers, proposed design principles, corrections, and review decisions.                           |
| [`docs/post-office-metadata-privacy.md`](post-office-metadata-privacy.md)           | Opaque relay mailboxes, visible routing metadata, security limits, and compatibility.                                                     |
| [`docs/post-office-release.md`](post-office-release.md)                             | Signed headless relay release archives and Linux GHCR container deployment.                                                               |
| [`docs/post-office-macos-service.md`](post-office-macos-service.md)                 | mac2015 relay service paths, Intel build, first install, update, and health commands.                                                     |
| [`docs/post-office-windows-service.md`](post-office-windows-service.md)             | Windows relay Scheduled Task, secure install, update, health, and LAN tests.                                                              |
| [`docs/backup-recovery.md`](backup-recovery.md)                                     | Manual encrypted backup, offline restore, recovery key handling, and coverage limits.                                                     |
| [`docs/channel-release-rehearsal.md`](channel-release-rehearsal.md)                 | Repeatable channel replica scale, old-binary compatibility, and physical LAN release checks.                                              |
| [`docs/updates.md`](updates.md)                                                     | Manual release check, verified package download, and platform install steps.                                                              |
| [`docs/privacy-controls.md`](privacy-controls.md)                                   | Privacy presets, app lock, invisible mode, hidden contacts, and limits.                                                                   |
| [`specifications/TODO.md`](../specifications/TODO.md)                               | Implementation status (feature-complete) + deliberate design notes / external constraints.                                                |

## Contributor workflow and design

| Doc                                                                                           | What it covers                                                                                    |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| [`AGENTS.md`](../AGENTS.md)                                                                   | Repository guidelines for AI agents / contributors — structure, build/test commands, conventions. |
| [`CONTRIBUTING.md`](../CONTRIBUTING.md)                                                       | How to set up, build, test, and submit changes.                                                   |
| [`specifications/development_conventions.md`](../specifications/development_conventions.md)   | Coding standards, naming, error handling, performance, cross-platform conventions.                |
| [`specifications/testing_guidelines.md`](../specifications/testing_guidelines.md)             | The three test layers — inline unit, multi-process backend E2E, Playwright UI E2E.                |
| [`docs/testing-feedback-loop.md`](testing-feedback-loop.md)                                   | Focused local test commands, module boundaries, and full-gate triggers.                           |
| [`specifications/git_standards.md`](../specifications/git_standards.md)                       | Conventional-commit format, scopes, and GPG signing / hook enforcement.                           |
| [`specifications/task_completion_criteria.md`](../specifications/task_completion_criteria.md) | Definition of done — the `check-health.sh` gate + docs/security requirements.                     |
| [`scripts/README.md`](../scripts/README.md)                                                   | Script entry points, CI globs, manual real-machine rigs, and maintenance tools.                   |
| [`docs/chat-ui-manual-test.md`](chat-ui-manual-test.md)                                       | Supplementary manual smoke checklist for the desktop chat UI.                                     |
| [`fuzz/README.md`](../fuzz/README.md)                                                         | Current protocol fuzz targets, scope, and local commands.                                         |
| [`frontend/DESIGN.md`](../frontend/DESIGN.md)                                                 | Current native-direction visual system and interaction rules.                                     |
| [`docs/screenshots/README.md`](screenshots/README.md)                                         | How current demo screenshots are captured and shared with the site.                               |
| [`docs/CUSTOMIZATION_PACKS.md`](CUSTOMIZATION_PACKS.md)                                       | ZIP pack format, Pack Studio, local installation, and GitHub Pages marketplace publishing.        |
| [`docs/DOCUMENTATION_GUIDELINES.md`](DOCUMENTATION_GUIDELINES.md)                             | Audience, source-of-truth rules, writing standards, and documentation review checklist.           |

## Evaluation records

These dated reports record what was tested and the limits of that evidence. Use
the current guides above for commands and implementation behavior.

| Doc                                                                                               | What it covers                                                               |
| ------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| [`docs/evals/architecture-score.md`](evals/architecture-score.md)                                 | Architecture review and the optimization series score.                       |
| [`docs/evals/architecture-boundaries-2026-10-15.md`](evals/architecture-boundaries-2026-10-15.md) | Feature responsibility map, dependency risks, and ordered refactoring steps. |
| [`docs/evals/delivery-ai-regression.md`](evals/delivery-ai-regression.md)                         | Delivery and AI validation contract.                                         |
| [`docs/evals/ai-eval-cases.md`](evals/ai-eval-cases.md)                                           | Stable AI eval and smoke cases.                                              |
| [`docs/evals/ai-eval-suite.json`](evals/ai-eval-suite.json)                                       | Machine-readable AI evaluation suite.                                        |
| [`docs/evals/linked-device-sync-ai-eval-suite.json`](evals/linked-device-sync-ai-eval-suite.json) | Model-backed linked-device review cases.                                     |
| [`docs/evals/module-coverage.json`](evals/module-coverage.json)                                   | Module coverage inventory used by the AI evaluation harness.                 |
| [`docs/evals/prompts/agent-contract-evaluator.md`](evals/prompts/agent-contract-evaluator.md)     | Evaluator prompt for agent-contract verdicts.                                |
| [`docs/evals/prompts/ui-ux-review.md`](evals/prompts/ui-ux-review.md)                             | UI review prompt used with screenshot evidence.                              |
| [`docs/evals/quality-gates.md`](evals/quality-gates.md)                                           | Review, mutation, and release evidence requirements.                         |
| [`docs/evals/core-platform-workflows.md`](evals/core-platform-workflows.md)                       | Backend, native, and browser evidence boundaries.                            |
| [`docs/evals/hidden-contacts.md`](evals/hidden-contacts.md)                                       | Hidden-contact native evidence.                                              |
| [`docs/evals/ui-ux-ai-review.md`](evals/ui-ux-ai-review.md)                                       | Screenshot-assisted UI review.                                               |
| [`docs/evals/file-transfer-experience.md`](evals/file-transfer-experience.md)                     | File transfer audit and validation.                                          |
| [`docs/evals/screenshot-capture-experience.md`](evals/screenshot-capture-experience.md)           | Screenshot interaction evaluation.                                           |
| [`docs/evals/channel-lifecycle-worktree.md`](evals/channel-lifecycle-worktree.md)                 | Channel lifecycle E2E evidence from its development worktree.                |

## Operations and deployment

| Doc                                                                                     | What it covers                                                                                           |
| --------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| [`specifications/deployment_guide.md`](../specifications/deployment_guide.md)           | Building/running the desktop app and the headless / post-office node on macOS, Windows, Linux.           |
| [`specifications/platform_permissions.md`](../specifications/platform_permissions.md)   | Per-platform entitlements, networking (UDP multicast 224.0.0.167:47474), notifications, at-rest secrets. |
| [`specifications/troubleshooting_guide.md`](../specifications/troubleshooting_guide.md) | Diagnosing discovery, connection, delivery, build, and platform issues.                                  |

## Security

| Doc                                                                                     | What it covers                                                                                                       |
| --------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| [`SECURITY.md`](../SECURITY.md)                                                         | Vulnerability-reporting policy and supported versions.                                                               |
| [`docs/ARCHITECTURE.md` § Security posture](ARCHITECTURE.md#8-security-posture-summary) | Crypto primitives, domain separation, forward secrecy, and known limitations by design.                              |
| [`docs/evals/managed-device-security-audit.md`](evals/managed-device-security-audit.md) | App-specific macOS managed-device exposure paths, risk ranking, and hardening sequence.                              |
| [`docs/evals/prompts/independent-review.md`](evals/prompts/independent-review.md)       | Clone-and-review steps plus a copyable English prompt for an evidence-based AI-assisted security and product review. |
| [`CODE_OF_CONDUCT.md`](../CODE_OF_CONDUCT.md)                                           | Community conduct expectations.                                                                                      |

The `docs/evals/` files record evidence at a point in time. For current behavior,
use the feature guides and architecture reference above, then verify against code.
Personal plans in `docs/superpowers/` and root `plans/` stay local.
