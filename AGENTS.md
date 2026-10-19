# Repository Guidelines

## Project Structure & Module Organization

- Four-crate Rust workspace. `crates/mesh-talk-core/` is the UI-free protocol core (lib
  `mesh_talk_core` + the `mesh-talk-node` CLI bin in `src/bin/`): feature modules `node/`,
  `identity/`, `transport/`, `discovery/`, `eventlog/`, `ratchet/`, `channel/`, `dm.rs`,
  `file/`, `postoffice/`, `storage/`. `src-tauri/` is the Tauri desktop app: entry
  `src/main.rs` → `lib.rs`, IPC glue in `commands.rs` (auth) + `chat_commands/` (messaging)
  - `events.rs`, plus `state.rs`, `tray.rs`, `services/` (auth only).
- `crates/mesh-talk-integrations/` owns scoped local messaging grants, private IPC,
  and the dedicated bot host/admin CLI; `crates/mesh-talk-mcp/` adapts its four
  send/context/status tools to the official MCP SDK. Neither owns mesh crypto.
- Integration/e2e suites live in `crates/mesh-talk-core/tests/` (`two_node_cli`,
  `persistent_history`, `post_office_offline`, `decoder_smoke`); keep unit tests inline via
  `mod tests` (e.g. `node/node_tests.rs`).
- React client sits in `frontend/src`; Vite tooling in `frontend/package.json`.
- Operational scripts in `scripts/` (`setup-hooks.sh`, `check-health.sh`); architecture in
  `docs/ARCHITECTURE.md`; conventions in `specifications/`. The
  [documentation map](docs/README.md) routes readers to maintained guides; the
  [documentation guidelines](docs/DOCUMENTATION_GUIDELINES.md) define ownership
  across the README, guides, and project site.

## Build, Test, and Development Commands

- `make dev` primes Rust + Node deps and installs git hooks.
- `make build` / `make frontend-build` — release build / Vite production build.
- During feature development, preview changed-boundary validation with `make test-plan`
  and run it with `make test-changed`. Unmapped changes require an explicit owner
  and reason; inspect escalation notes for browser/native or consumer checks.
  Use `make verify-plan` / `make verify-changed` at task completion to compose the
  same affected tests with applicable formatting, lint/types and builds. The
  test-only commands do not include static checks; neither mode replaces required
  PR/review, browser/native or release gates. `make validation-report` separates
  queue and execution costs; `make validation-audit` compares a clean affected
  plan with same-source broader failure evidence before narrowing any gate.
  Read selected capability references and inspect pending browser/native requirements.
  Execution prints a structured feedback report; follow the
  [validation protocol](docs/testing-feedback-loop.md#validation-stages-and-agent-responsibilities)
  for completed work, remaining evidence and additive semantic selections.
- During feature development, run affected tests only: `make test-focused
TEST_PACKAGE=mesh-talk-core TEST_FILTER=node::account_sync` selects Rust unit
  tests; add `TEST_TARGET=bridge_cross_lan` to select one integration target.
  `cd frontend && npm test -- src/features/chat/settingsDialogSession.test.ts`
  selects a frontend unit file. `make test` runs the full workspace suite
  (`cargo test --workspace`) for pre-PR and CI gates;
  `make e2e` runs the `#[ignore]`d multi-process backend rigs; `cd frontend && npm run e2e`
  runs the Playwright UI suite. Test builds use cheap KDF params (the `fast-test-kdf`
  feature), and `.cargo/config.toml` caps test/compile parallelism.
- UI / node spot checks: `make tauri-dev`, `make frontend-dev`,
  `cargo run -p mesh-talk-core --bin mesh-talk-node -- --keystore /tmp/a.keystore
--password pw --name alice` (add `--post-office` for relay mode).

## Coding Style & Naming Conventions

- Rust: `cargo fmt` (4-space, `snake_case` modules, `UpperCamelCase` types) +
  **`cargo clippy --all-targets -- -D warnings`** (must match CI — `--all-targets` covers
  tests/benches).
- Frontend: `npm run format` (Prettier) + `npm run lint` (ESLint flat config).
- Write documentation and reusable prompts in English unless a specific exception is needed.
- Reference `specifications/development_conventions.md` for naming nuances.

## Domain Language

- `CONTEXT.md` is the canonical domain glossary. `docs/domain-design.md` indexes
  the modeling references and defines DDD vocabulary and naming translations.
- Distinguish local users, accounts and devices; desktop, transport and ratchet
  sessions; discovery, presence, custody, recipient delivery and read state.
- Use explicit identity/lifetime names in new code. Preserve public, wire and
  stored identifiers unless a separately validated compatibility change is intended.
- Update the glossary and affected owning guides when a domain meaning changes;
  link to definitions instead of maintaining duplicate glossaries.

## Architecture & Components

- Before adding business logic, consult [module ownership and reuse](docs/module-boundaries.md),
  inspect the owning module and its callers, and extend its existing implementation.
  Parallel tasks should name their owned paths and shared-interface dependencies;
  coordinate changes to shared interfaces through one integration owner. Keep
  feature-specific helpers local and share rules when their meaning and lifecycle
  are the same. The guide owns the detailed assignment and handoff contract.
- Stack: Rust 2024 on Tokio; Tauri 2 + React shell. No vendor-operated server.
- The `Node` (`node/node.rs`) wires identity + signed UDP **multicast** discovery
  (group `224.0.0.167`, port 47474) + Noise-encrypted TCP transport (`transport/`,
  low-level socket in `transport/net.rs`) + the content-addressed event log + DM/channel/
  file crypto. Messages are events synced as a bounded append-only log, account-addressed
  across a user's devices; optional offline custody via equal eligible post offices.
  Routed discovery and opt-in post-office federation connect reachable sites without
  joining their IP subnets. Wire formats are versioned bincode.
- See **`docs/ARCHITECTURE.md`** before changing crypto, sync, or transport;
  **`docs/network-design-reference.md`** owns the communication invariants,
  module contracts, resource budgets and topology acceptance boundaries.

## Testing Guidelines

- Integration coverage in `crates/mesh-talk-core/tests/` and `node/node_tests.rs`; embed
  unit tests near the code. Networking/crypto changes ship a loopback integration assertion.
- Backend multi-process E2E via `make e2e` (CI: `e2e-backend.yml`); Playwright UI E2E via
  `cd frontend && npm run e2e` (CI: `e2e-ui.yml`).
- Do not run full suites or E2E automatically during ordinary feature work. Run
  the affected unit or integration tests as changes are made; run E2E and the
  comprehensive health gate on explicit request, before PR/review, or in CI.
- See `docs/testing-feedback-loop.md` for selection examples and escalation triggers.
- Document executed validation commands in every PR (minimum `make test`).

## Commit & Pull Request Guidelines

- Conventional commits (`feat:`, `fix:`, `refactor:`); scopes like `feat(node):` align with
  `specifications/git_standards.md`. Squash WIP; link issues.
- Create PRs through `./scripts/create-pr.sh <preflight selectors/specs> -- <gh pr create options>`.
  It runs `prepare-pr.sh` (full health plus platform preflight) and refuses to publish if verification fails or the candidate changes.
  Before opening or updating a PR, commit the branch and run `./scripts/prepare-pr.sh` with only the
  Playwright specs relevant to changed frontend behavior (for example,
  `./scripts/prepare-pr.sh e2e/avatar-gallery.spec.ts e2e/theme-picker.spec.ts`).
  Preflight automatically runs the same selected tests in Linux Docker and through SSH to
  `windows10`; Windows workspaces, caches and temporary data stay on drive D. For changed
  Rust crates, pass repeated `--core-filter <test filter>` / `--host-filter <test filter>`
  selectors; use `--integration-filter` and `--mcp-filter` for those crates.
  Owning scenario rules automatically add visual specs and require real native
  evidence for lifecycle/discovery changes. Missing environments or empty Rust selections fail the gate; do not silently
  skip them. See `docs/testing-feedback-loop.md` for configuration and plan preview.
  Include `e2e/ui-visual-regression.spec.ts` when a change affects theme layout or snapshots.
  It checks the changed-line coverage estimate and portable scenario evidence; add focused
  tests or specs named by its uncovered-line report before broadening the run. Fetch
  `origin/main` first if the base ref is stale. Keep the full suite for the final gate.
- Request review only after `./scripts/check-health.sh` passes (it mirrors CI exactly).

## Security & Adjustments

- `./scripts/check-health.sh` chains fmt, Clippy `--all-targets`, ESLint, tests, typos,
  cargo-deny, cargo-machete, gitleaks, shellcheck, audits, and both builds.
- Triage vulnerabilities immediately or document mitigation in the PR.
- If plans shift, update the relevant committed design or architecture docs. Keep
  `docs/superpowers/` and the root `plans/` directory local and untracked.

## Repository Hygiene

- Keep implementation plans, research, investigation notes and scratch scripts
  under ignored `tmp/`; remove them when the work is complete.
- Merge durable decisions into the maintained owning guide. Do not commit
  completed-work reports, score histories, duplicate runbooks or archive copies.
- `docs/evals/` owns reusable suites/inventories, prompts and its README only.
  Generated evidence belongs in ignored output or PR/CI artifacts.
- Run `node scripts/check-repository-hygiene.mjs` before committing. The shared
  index check also runs in pre-commit, CI and health, rejecting ignored files
  even when force-added. See `docs/DOCUMENTATION_GUIDELINES.md` for ownership.
