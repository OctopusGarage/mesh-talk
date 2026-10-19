# Documentation

For delayed group messages, unread behavior and recovery status, see
[group conversation reception](group-conversation-reception.md). Its
[research note](research/group-message-reception-ux.md) records the protocol and
UX rationale.

This is the navigation hub for Mesh-Talk's current guides, technical references, and
dated evidence. Start with the path that matches your task. The
[documentation guidelines](DOCUMENTATION_GUIDELINES.md) explain where new information
belongs and how to keep the site, README, and guides aligned.

| I want to…                         | Start here                                                   | Continue with                                                                                                                                                                           |
| ---------------------------------- | ------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Install or try the desktop app     | [Project overview](../README.md)                             | [Install and update](updates.md) · [Troubleshooting](../specifications/troubleshooting_guide.md)                                                                                        |
| Connect peers across networks      | [Network architecture baseline](network-design-reference.md) | [Current topologies](network-topologies.md) · [Scenario Q&A](network-design-qa.md) · [Routed discovery](routed-network-discovery.md) · [Relay privacy](post-office-metadata-privacy.md) |
| Run a headless node or relay       | [Deployment](../specifications/deployment_guide.md)          | [Relay releases](post-office-release.md)                                                                                                                                                |
| Understand or change the protocol  | [Architecture](ARCHITECTURE.md)                              | [Domain model](../CONTEXT.md) · [Testing feedback loop](testing-feedback-loop.md)                                                                                                       |
| Contribute to the desktop app      | [Contributing](../CONTRIBUTING.md)                           | [Visual system](../frontend/DESIGN.md) · [Customization packs](CUSTOMIZATION_PACKS.md)                                                                                                  |
| Review security or past validation | [Security policy](../SECURITY.md)                            | [Architecture security posture](ARCHITECTURE.md#8-security-posture-summary) · [Evaluation records](#evaluation-records)                                                                 |

For menu locations and everyday tasks, start with the [user guide](help/README.md).
The [help system guide](help-system.md) explains offline reading, publication and
the read-only [documentation MCP](../tools/help/README.md).

## Current product and architecture

| Doc                                                                                 | What it covers                                                                                                                                              |
| ----------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`README.md`](../README.md)                                                         | Project overview, install, quick start.                                                                                                                     |
| [`PROJECT.md`](../PROJECT.md)                                                       | Top-level context pointer for AI tools / new contributors.                                                                                                  |
| [`CONTEXT.md`](../CONTEXT.md)                                                       | Domain model — entities, layers, invariants, what the project does _not_ own.                                                                               |
| [`docs/ARCHITECTURE.md`](ARCHITECTURE.md)                                           | Authoritative technical reference — crypto, event log + sync, transport, discovery, post office, build/test/CI.                                             |
| [`docs/routed-network-discovery.md`](routed-network-discovery.md)                   | Optional routed discovery over existing Tailscale, EasyTier or manually supplied addresses, identity boundary, route selection, and release checks.         |
| [`docs/routed-network-discovery-research.md`](routed-network-discovery-research.md) | Historical cited research and design options behind routed discovery; see the implementation guide for current behavior.                                    |
| [`docs/easytier-compatibility-research.md`](easytier-compatibility-research.md)     | Dated EasyTier feasibility research, discovery integration options, source evidence, and validation limits.                                                 |
| [Incoming DM language](dm-language-restrictions.md)                                 | Configure incoming message languages, 80/20 mixed-language tolerance, local detection, and device compatibility.                                            |
| [DM language restriction research](dm-language-restriction-research.md)             | Proposed selectable DM language restrictions, 80/20 mixed-language tolerance, local validation, and compatibility boundaries; translation is excluded.      |
| [`docs/network-design-reference.md`](network-design-reference.md)                   | Network Topology and Communication Architecture: principles, module contracts, settings, 34 illustrated scenarios, resource limits and acceptance evidence. |
| [`docs/network-topologies.md`](network-topologies.md)                               | Review baseline: network/data planes, switch matrix, LAN/Tailscale/federation diagrams, synchronization paths, and evidence limits.                         |
| [`docs/network-design-qa.md`](network-design-qa.md)                                 | Focused questions and current answers; remote-relay participation, group chains and links to the architectural baseline.                                    |
| [`docs/post-office-federation.md`](post-office-federation.md)                       | Equal-relay trust manifest, enrollment/revocation, boot-service deployment, compatibility, and bounded discovery.                                           |
| [`docs/post-office-metadata-privacy.md`](post-office-metadata-privacy.md)           | Opaque relay mailboxes, visible routing metadata, security limits, and compatibility.                                                                       |
| [`docs/post-office-release.md`](post-office-release.md)                             | Signed headless relay release archives and Linux GHCR container deployment.                                                                                 |
| [`docs/post-office-macos-service.md`](post-office-macos-service.md)                 | mac2015 relay service paths, Intel build, first install, update, and health commands.                                                                       |
| [`docs/post-office-windows-service.md`](post-office-windows-service.md)             | Windows relay Scheduled Task, secure install, update, health, and LAN tests.                                                                                |
| [`docs/backup-recovery.md`](backup-recovery.md)                                     | Manual encrypted backup, offline restore, recovery key handling, and coverage limits.                                                                       |
| [`docs/channel-release-rehearsal.md`](channel-release-rehearsal.md)                 | Repeatable channel replica scale, old-binary compatibility, and physical LAN release checks.                                                                |
| [`docs/updates.md`](updates.md)                                                     | Manual release check, verified package download, and platform install steps.                                                                                |
| [`docs/settings.md`](settings.md)                                                   | Seven category pages, automatic saving, permission dependencies, local retention confirmation and settings ownership.                                       |
| [`docs/privacy-controls.md`](privacy-controls.md)                                   | Privacy presets, app lock, invisible mode, hidden contacts, and limits.                                                                                     |
| [`specifications/TODO.md`](../specifications/TODO.md)                               | Current capability index, deliberate design boundaries and separately unverified acceptance cases.                                                          |

## Bots and local agents

[Bot integrations](bot-integrations.md) explains the dedicated headless bot host,
opt-in desktop delegation, scoped CLI credentials and local MCP tools. Text/reply
sending is implemented; interactive subscriptions and HTTP ingress are later stages.

## Proposed features and research

These documents distinguish implemented initial capabilities from investigated
options and proposed later stages. The usage guide above owns current behavior.

| Doc                                                                    | What it covers                                                                                                                                                       |
| ---------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Bots and MCP architecture proposal](bot-mcp-architecture-research.md) | Bot identities, group installations, desktop and headless hosting, shared integration service, reliable sending, MCP/webhook adapters, security and staged delivery. |
| [Feishu/Lark and MCP source research](bot-mcp-source-research.md)      | Official bot/API/SDK references, current MCP version and transport requirements, and evidence limits.                                                                |

## Contributor workflow and design

| Doc                                                                                           | What it covers                                                                                           |
| --------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| [`AGENTS.md`](../AGENTS.md)                                                                   | Repository guidelines for AI agents / contributors — structure, build/test commands, conventions.        |
| [`CONTRIBUTING.md`](../CONTRIBUTING.md)                                                       | How to set up, build, test, and submit changes.                                                          |
| [`docs/module-boundaries.md`](module-boundaries.md)                                           | Maintained feature owners, reuse seams, sidebar modules and coordination rules for parallel development. |
| [`specifications/development_conventions.md`](../specifications/development_conventions.md)   | Coding standards, naming, error handling, performance, cross-platform conventions.                       |
| [`specifications/testing_guidelines.md`](../specifications/testing_guidelines.md)             | The three test layers — inline unit, multi-process backend E2E, Playwright UI E2E.                       |
| [`docs/testing-feedback-loop.md`](testing-feedback-loop.md)                                   | Focused local test commands, module boundaries, and full-gate triggers.                                  |
| [`docs/build-retention.md`](build-retention.md)                                               | Shared build archives, compiler-cache budget, worktree discovery, and cleanup coordination.              |
| [`specifications/git_standards.md`](../specifications/git_standards.md)                       | Conventional-commit format, scopes, and GPG signing / hook enforcement.                                  |
| [`specifications/task_completion_criteria.md`](../specifications/task_completion_criteria.md) | Definition of done — the `check-health.sh` gate + docs/security requirements.                            |
| [`scripts/README.md`](../scripts/README.md)                                                   | Script entry points, CI globs, manual real-machine rigs, and maintenance tools.                          |
| [`docs/chat-ui-manual-test.md`](chat-ui-manual-test.md)                                       | Supplementary manual smoke checklist for the desktop chat UI.                                            |
| [`fuzz/README.md`](../fuzz/README.md)                                                         | Current protocol fuzz targets, scope, and local commands.                                                |
| [`frontend/DESIGN.md`](../frontend/DESIGN.md)                                                 | Current native-direction visual system and interaction rules.                                            |
| [`docs/screenshots/README.md`](screenshots/README.md)                                         | How current demo screenshots are captured and shared with the site.                                      |
| [`docs/CUSTOMIZATION_PACKS.md`](CUSTOMIZATION_PACKS.md)                                       | ZIP pack format, Pack Studio, local installation, and GitHub Pages marketplace publishing.               |
| [`docs/DOCUMENTATION_GUIDELINES.md`](DOCUMENTATION_GUIDELINES.md)                             | Audience, source-of-truth rules, writing standards, and documentation review checklist.                  |

## Proposed designs and research

These documents distinguish inspected behavior from proposed changes. They do not
establish shipped capabilities or completed validation.

| Doc                                                                             | What it covers                                                                                                             |
| ------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| [Multi-device synchronization research](research/multi-device-sync-patterns.md) | Dated primary-source comparison of Signal, Matrix, Syncthing and document CRDTs.                                           |
| [Personal messaging and synchronization design](multi-device-sync-design.md)    | Existing-code gaps, recommended models/protocols, conflict rules, settings, offline limits and device-security boundaries. |
| [Synchronization implementation plan](multi-device-sync-plan.md)                | Sequenced work packages, file owners, focused tests, compatibility and release gates.                                      |

## Evaluation records

[Feature responsibility and reuse review — 2026-10-19](evals/feature-responsibility-review-2026-10-19.md) maps feature owners, existing reuse, justified component extractions, and selective validation. It records one coordination hotspot and three smaller reuse opportunities.

[Bot integration implementation evidence, 2026-10-19](evals/bot-integrations-2026-10-19.md) records focused test commands, standalone binary checks and platform limits.

These dated reports record what was tested and the limits of that evidence. Use
the current guides above for commands and implementation behavior.

EasyTier-specific evidence: [two-Mac acceptance, 2026-10-17](evals/easytier-macos-2026-10-17.md).

| Doc                                                                                               | What it covers                                                                                       |
| ------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| [`docs/evals/tray-menu.md`](evals/tray-menu.md)                                                   | Tray navigation regressions, native menu evidence and validation limits.                             |
| [`docs/evals/tray-menu-ai-eval-suite.json`](evals/tray-menu-ai-eval-suite.json)                   | Source evaluation cases for native tray and utility dialog ownership.                                |
| [`docs/evals/settings-pane-review-2026-10-16.md`](evals/settings-pane-review-2026-10-16.md)       | Settings inventory, source/browser findings, proposed categories and redesign acceptance boundaries. |
| [`docs/evals/architecture-score.md`](evals/architecture-score.md)                                 | Current fixed-rubric architecture health loop and historical assessments.                            |
| [`docs/evals/architecture-boundaries-2026-10-15.md`](evals/architecture-boundaries-2026-10-15.md) | Feature responsibility map, dependency risks, and ordered refactoring steps.                         |
| [`docs/evals/delivery-ai-regression.md`](evals/delivery-ai-regression.md)                         | Delivery and AI validation contract.                                                                 |
| [`docs/evals/ai-eval-cases.md`](evals/ai-eval-cases.md)                                           | Stable AI eval and smoke cases.                                                                      |
| [`docs/evals/ai-eval-suite.json`](evals/ai-eval-suite.json)                                       | Machine-readable AI evaluation suite.                                                                |
| [`docs/evals/linked-device-sync-ai-eval-suite.json`](evals/linked-device-sync-ai-eval-suite.json) | Model-backed linked-device review cases.                                                             |
| [`docs/evals/module-coverage.json`](evals/module-coverage.json)                                   | Module coverage inventory used by the AI evaluation harness.                                         |
| [`docs/evals/prompts/agent-contract-evaluator.md`](evals/prompts/agent-contract-evaluator.md)     | Evaluator prompt for agent-contract verdicts.                                                        |
| [`docs/evals/prompts/ui-ux-review.md`](evals/prompts/ui-ux-review.md)                             | UI review prompt used with screenshot evidence.                                                      |
| [`docs/evals/quality-gates.md`](evals/quality-gates.md)                                           | Review, mutation, and release evidence requirements.                                                 |
| [`docs/evals/core-platform-workflows.md`](evals/core-platform-workflows.md)                       | Backend, native, and browser evidence boundaries.                                                    |
| [`docs/evals/hidden-contacts.md`](evals/hidden-contacts.md)                                       | Hidden-contact native evidence.                                                                      |
| [`docs/evals/ui-ux-ai-review.md`](evals/ui-ux-ai-review.md)                                       | Screenshot-assisted UI review.                                                                       |
| [`docs/evals/file-transfer-experience.md`](evals/file-transfer-experience.md)                     | File transfer audit and validation.                                                                  |
| [`docs/evals/screenshot-capture-experience.md`](evals/screenshot-capture-experience.md)           | Screenshot interaction evaluation.                                                                   |
| [`docs/evals/channel-lifecycle-worktree.md`](evals/channel-lifecycle-worktree.md)                 | Channel lifecycle E2E evidence from its development worktree.                                        |

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
