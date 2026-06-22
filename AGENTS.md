# Repository Guidelines

## Project Structure & Module Organization
- Two-crate Rust workspace. `crates/mesh-talk-core/` is the UI-free protocol core (lib
  `mesh_talk_core` + the `mesh-talk-node` CLI bin in `src/bin/`): feature modules `node/`,
  `identity/`, `transport/`, `discovery/`, `eventlog/`, `ratchet/`, `channel/`, `dm.rs`,
  `file/`, `postoffice/`, `storage/`. `src-tauri/` is the Tauri desktop app: entry
  `src/main.rs` → `lib.rs`, IPC glue in `commands.rs` (auth) + `chat_commands.rs` (messaging)
  + `events.rs`, plus `state.rs`, `tray.rs`, `services/` (auth only).
- Integration/e2e suites live in `crates/mesh-talk-core/tests/` (`two_node_cli`,
  `persistent_history`, `post_office_offline`, `decoder_smoke`); keep unit tests inline via
  `mod tests` (e.g. `node/node_tests.rs`).
- React client sits in `frontend/src`; Vite tooling in `frontend/package.json`.
- Operational scripts in `scripts/` (`setup-hooks.sh`, `check-health.sh`); architecture in
  `docs/ARCHITECTURE.md`; conventions in `specifications/`. All docs are indexed in
  [`docs/README.md`](docs/README.md) (documentation map).

## Build, Test, and Development Commands
- `make dev` primes Rust + Node deps and installs git hooks.
- `make build` / `make frontend-build` — release build / Vite production build.
- Three test layers: `make test` runs the workspace unit/integration suite
  (`cargo test --workspace`; filter with `cargo test -p mesh-talk-core --lib node::node`);
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
- Reference `specifications/development_conventions.md` for naming nuances.

## Architecture & Components
- Stack: Rust 2021 on Tokio; Tauri 2 + React shell. No server.
- The `Node` (`node/node.rs`) wires identity + signed UDP **multicast** discovery
  (group `224.0.0.167`, port 47474) + Noise-encrypted TCP transport (`transport/`,
  low-level socket in `transport/net.rs`) + the content-addressed event log + DM/channel/
  file crypto. Messages are events synced as a bounded append-only log, account-addressed
  across a user's devices; offline delivery via an elected post office. Wire formats are
  versioned bincode.
- See **`docs/ARCHITECTURE.md`** before changing crypto, sync, or transport.

## Testing Guidelines
- Integration coverage in `crates/mesh-talk-core/tests/` and `node/node_tests.rs`; embed
  unit tests near the code. Networking/crypto changes ship a loopback integration assertion.
- Backend multi-process E2E via `make e2e` (CI: `e2e-backend.yml`); Playwright UI E2E via
  `cd frontend && npm run e2e` (CI: `e2e-ui.yml`).
- Document executed validation commands in every PR (minimum `make test`).

## Commit & Pull Request Guidelines
- Conventional commits (`feat:`, `fix:`, `refactor:`); scopes like `feat(node):` align with
  `specifications/git_standards.md`. Squash WIP; link issues.
- Request review only after `./scripts/check-health.sh` passes (it mirrors CI exactly).

## Security & Adjustments
- `./scripts/check-health.sh` chains fmt, Clippy `--all-targets`, ESLint, tests, typos,
  cargo-deny, cargo-machete, gitleaks, shellcheck, audits, and both builds.
- Triage vulnerabilities immediately or document mitigation in the PR.
- If plans shift, update the design docs under `docs/superpowers/` and `docs/ARCHITECTURE.md`.
