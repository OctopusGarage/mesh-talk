# Documentation

This is the navigation hub for Mesh-Talk's current guides, technical references, and
validation guidance. Start with the path that matches your task. The
[documentation guidelines](DOCUMENTATION_GUIDELINES.md) explain where new information
belongs and how to keep the site, README, and guides aligned.

| I want to…                                    | Start here                                                   | Continue with                                                                                                                                    |
| --------------------------------------------- | ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| Install or try the desktop app                | [Project overview](../README.md)                             | [Install and update](updates.md) · [Troubleshooting](../specifications/troubleshooting_guide.md)                                                 |
| Read conversations or resolve missing history | [User guide](help/README.md)                                 | [Group reception](group-conversation-reception.md) · [Message delivery](help/topics/delivery.md) · [Files](help/topics/send-files.md)            |
| Link devices or recover an account            | [Linked devices](help/topics/link-devices.md)                | [Backup and recovery](backup-recovery.md) · [Settings](settings.md)                                                                              |
| Connect a bot or local assistant              | [Bot integrations](bot-integrations.md)                      | [Public documentation MCP](../tools/help/README.md)                                                                                              |
| Connect peers across networks                 | [Network architecture baseline](network-design-reference.md) | [Current topologies](network-topologies.md) · [Routed discovery](routed-network-discovery.md) · [Relay privacy](post-office-metadata-privacy.md) |
| Run a headless node or relay                  | [Deployment](../specifications/deployment_guide.md)          | [Relay releases](post-office-release.md)                                                                                                         |
| Understand or change the protocol             | [Architecture](ARCHITECTURE.md)                              | [Domain glossary](../CONTEXT.md) · [DDD conventions](domain-design.md) · [Testing feedback loop](testing-feedback-loop.md)                       |
| Contribute to the desktop app                 | [Contributing](../CONTRIBUTING.md)                           | [Visual system](../frontend/DESIGN.md) · [Customization packs](CUSTOMIZATION_PACKS.md)                                                           |
| Review security or past validation            | [Security policy](../SECURITY.md)                            | [Architecture security posture](ARCHITECTURE.md#8-security-posture-summary) · [Validation and evaluation](#validation-and-evaluation)            |

For menu locations and everyday tasks, start with the [user guide](help/README.md).
The [help system guide](help-system.md) explains offline reading, publication and
the read-only [documentation MCP](../tools/help/README.md).

## Current product and architecture

| Doc                                                                       | What it covers                                                                                                                                              |
| ------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`README.md`](../README.md)                                               | Project overview, install, quick start.                                                                                                                     |
| [`PROJECT.md`](../PROJECT.md)                                             | Top-level context pointer for AI tools / new contributors.                                                                                                  |
| [`CONTEXT.md`](../CONTEXT.md)                                             | Canonical domain glossary — identity, conversations, networking, acceptance and delivery.                                                                   |
| [`docs/domain-design.md`](domain-design.md)                               | DDD vocabulary, reference index, identifier translations and conventions for code and docs.                                                                 |
| [`docs/ARCHITECTURE.md`](ARCHITECTURE.md)                                 | Authoritative technical reference — crypto, event log + sync, transport, discovery, post office, build/test/CI.                                             |
| [`docs/routed-network-discovery.md`](routed-network-discovery.md)         | Optional routed discovery over existing Tailscale, EasyTier or manually supplied addresses, identity boundary, route selection, and release checks.         |
| [`docs/network-design-reference.md`](network-design-reference.md)         | Network Topology and Communication Architecture: principles, module contracts, settings, 34 illustrated scenarios, resource limits and acceptance evidence. |
| [`docs/network-topologies.md`](network-topologies.md)                     | Network/data planes, switch matrix, LAN/Tailscale/federation diagrams, synchronization paths, and evidence limits.                                          |
| [`docs/post-office-federation.md`](post-office-federation.md)             | Equal-relay trust manifest, enrollment/revocation, boot-service deployment, compatibility, and bounded discovery.                                           |
| [`docs/post-office-metadata-privacy.md`](post-office-metadata-privacy.md) | Opaque relay mailboxes, visible routing metadata, security limits, and compatibility.                                                                       |
| [`docs/post-office-release.md`](post-office-release.md)                   | Signed headless relay release archives and Linux GHCR container deployment.                                                                                 |
| [`docs/post-office-macos-service.md`](post-office-macos-service.md)       | mac2015 relay service paths, Intel build, first install, update, and health commands.                                                                       |
| [`docs/post-office-windows-service.md`](post-office-windows-service.md)   | Windows relay Scheduled Task, secure install, update, health, and LAN tests.                                                                                |
| [`docs/backup-recovery.md`](backup-recovery.md)                           | Manual encrypted backup, offline restore, recovery key handling, and coverage limits.                                                                       |
| [`docs/channel-release-rehearsal.md`](channel-release-rehearsal.md)       | Repeatable channel replica scale, old-binary compatibility, and physical LAN release checks.                                                                |
| [`docs/updates.md`](updates.md)                                           | Stable/Nightly selection, verified downloads, native installation, canonical product-version ownership, reviewed release/next-target automation, and publication recovery. |
| [`docs/settings.md`](settings.md)                                         | Seven category pages, automatic saving, permission dependencies, local retention confirmation and settings ownership.                                       |
| [`docs/privacy-controls.md`](privacy-controls.md)                         | Privacy presets, app lock, invisible mode, hidden contacts, and limits.                                                                                     |
| [`specifications/TODO.md`](../specifications/TODO.md)                     | Current capability index, deliberate design boundaries and separately unverified acceptance cases.                                                          |

The [group reception guide](group-conversation-reception.md) owns ordering, unread
tracking, catch-up notifications and local recovery observations. For embedding
the UI-free protocol core, see the [core crate guide](../crates/mesh-talk-core/README.md).

[Peer trust cards](peer-trust.md) explains signed public profiles, creation claims,
independent witness receipts and private relationship evidence.

## Bots and local agents

[Bot integrations](bot-integrations.md) explains the dedicated headless bot host,
opt-in desktop delegation, scoped CLI credentials and local MCP tools. Text/reply
sending is implemented; interactive subscriptions and HTTP ingress are later stages.

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
| [`fuzz/README.md`](../fuzz/README.md)                                                         | Current protocol fuzz targets, scope, and local commands.                                                |
| [`frontend/DESIGN.md`](../frontend/DESIGN.md)                                                 | Current native-direction visual system and interaction rules.                                            |
| [`docs/screenshots/README.md`](screenshots/README.md)                                         | How current demo screenshots are captured and shared with the site.                                      |
| [`docs/CUSTOMIZATION_PACKS.md`](CUSTOMIZATION_PACKS.md)                                       | ZIP pack format, Pack Studio, local installation, and GitHub Pages marketplace publishing.               |
| [`docs/DOCUMENTATION_GUIDELINES.md`](DOCUMENTATION_GUIDELINES.md)                             | Audience, source-of-truth rules, writing standards, and documentation review checklist.                  |

## Design boundaries

[Personal messaging and synchronization](multi-device-sync-design.md) retains the
product contract and distinguishes proposed private-state and authority extensions
from current behavior. The architecture and feature guides own shipped behavior. Completed first-release
audits and implementation steps remain in Git history.

## Validation and evaluation

The [evaluation registry](evals/README.md) owns reusable cases, suites and prompts.
[Platform validation](platform-validation.md) distinguishes backend, native,
browser and model evidence. Keep generated reports, screenshots from investigations
and completed-work logs in ignored local output or PR/CI artifacts. Git history
retains retired records without presenting them as current documentation.

| Doc                                                                                               | What it covers                                                        |
| ------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| [`docs/evals/tray-menu-ai-eval-suite.json`](evals/tray-menu-ai-eval-suite.json)                   | Source evaluation cases for native tray and utility dialog ownership. |
| [`docs/evals/README.md`](evals/README.md)                                                         | Stable AI eval and smoke cases.                                       |
| [`docs/evals/ai-eval-suite.json`](evals/ai-eval-suite.json)                                       | Machine-readable AI evaluation suite.                                 |
| [`docs/evals/linked-device-sync-ai-eval-suite.json`](evals/linked-device-sync-ai-eval-suite.json) | Model-backed linked-device review cases.                              |
| [`docs/evals/module-coverage.json`](evals/module-coverage.json)                                   | Module coverage inventory used by the AI evaluation harness.          |
| [`docs/evals/prompts/agent-contract-evaluator.md`](evals/prompts/agent-contract-evaluator.md)     | Evaluator prompt for agent-contract verdicts.                         |
| [`docs/evals/prompts/ui-ux-review.md`](evals/prompts/ui-ux-review.md)                             | UI review prompt used with screenshot evidence.                       |
| [`docs/quality-gates.md`](quality-gates.md)                                                       | Review, mutation, and release evidence requirements.                  |
| [`docs/platform-validation.md`](platform-validation.md)                                           | Backend, native, and browser evidence boundaries.                     |

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
| [`docs/evals/prompts/independent-review.md`](evals/prompts/independent-review.md)       | Clone-and-review steps plus a copyable English prompt for an evidence-based AI-assisted security and product review. |
| [`CODE_OF_CONDUCT.md`](../CODE_OF_CONDUCT.md)                                           | Community conduct expectations.                                                                                      |

The `docs/evals/` directory contains maintained evaluation inputs. Feature guides
and the architecture reference own current behavior; verify changes against code.
Keep temporary plans, investigations and research under ignored `tmp/`.
Promote lasting decisions into the owning guide; Git history retains retired reports.
