# Mesh-Talk Documentation Map

An index of every living doc in the repository, grouped by purpose. Mesh-Talk is a
serverless, end-to-end-encrypted, local-first P2P LAN messenger (Tauri + React desktop app
plus a headless `mesh-talk-node` CLI over a shared Rust core).

> New here? Start with the root [`README.md`](../README.md), then
> [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Overview & Architecture

| Doc | What it covers |
|-----|----------------|
| [`README.md`](../README.md) | Project overview, install, quick start. |
| [`PROJECT.md`](../PROJECT.md) | Top-level context pointer for AI tools / new contributors. |
| [`CONTEXT.md`](../CONTEXT.md) | Domain model — entities, layers, invariants, what the project does *not* own. |
| [`docs/ARCHITECTURE.md`](ARCHITECTURE.md) | Authoritative technical reference — crypto, event log + sync, transport, discovery, post office, build/test/CI. |
| [`specifications/TODO.md`](../specifications/TODO.md) | Implementation status (feature-complete) + deliberate design notes / external constraints. |

## Development

| Doc | What it covers |
|-----|----------------|
| [`AGENTS.md`](../AGENTS.md) | Repository guidelines for AI agents / contributors — structure, build/test commands, conventions. |
| [`CONTRIBUTING.md`](../CONTRIBUTING.md) | How to set up, build, test, and submit changes. |
| [`specifications/development_conventions.md`](../specifications/development_conventions.md) | Coding standards, naming, error handling, performance, cross-platform conventions. |
| [`specifications/testing_guidelines.md`](../specifications/testing_guidelines.md) | The three test layers — inline unit, multi-process backend E2E, Playwright UI E2E. |
| [`specifications/git_standards.md`](../specifications/git_standards.md) | Conventional-commit format, scopes, and GPG signing / hook enforcement. |
| [`specifications/task_completion_criteria.md`](../specifications/task_completion_criteria.md) | Definition of done — the `check-health.sh` gate + docs/security requirements. |
| [`docs/chat-ui-manual-test.md`](chat-ui-manual-test.md) | Supplementary manual smoke checklist for the desktop chat UI. |

## Operations & Deployment

| Doc | What it covers |
|-----|----------------|
| [`specifications/deployment_guide.md`](../specifications/deployment_guide.md) | Building/running the desktop app and the headless / post-office node on macOS, Windows, Linux. |
| [`specifications/platform_permissions.md`](../specifications/platform_permissions.md) | Per-platform entitlements, networking (UDP multicast 224.0.0.167:47474), notifications, at-rest secrets. |
| [`specifications/troubleshooting_guide.md`](../specifications/troubleshooting_guide.md) | Diagnosing discovery, connection, delivery, build, and platform issues. |

## Security

| Doc | What it covers |
|-----|----------------|
| [`SECURITY.md`](../SECURITY.md) | Vulnerability-reporting policy and supported versions. |
| [`docs/ARCHITECTURE.md` § Security posture](ARCHITECTURE.md#8-security-posture-summary) | Crypto primitives, domain separation, forward secrecy, and known limitations by design. |
| [`CODE_OF_CONDUCT.md`](../CODE_OF_CONDUCT.md) | Community conduct expectations. |

## Design history

Per-phase design specs and implementation plans live under
[`docs/superpowers/specs/`](superpowers/specs/) and
[`docs/superpowers/plans/`](superpowers/plans/). Detailed task history lives in git.
