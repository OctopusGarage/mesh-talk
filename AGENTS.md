# Repository Guidelines

## Project Structure & Module Organization
- Backend Rust workspace `src-tauri/`; entry `src-tauri/src/main.rs`, core `lib.rs`, feature modules under `domain/`, `services/`, `network/`, with CLI wiring in `api.rs` and events/command glue in `events.rs`, `commands.rs`.
- Integration/e2e suites live in `src-tauri/tests/`; keep unit tests inline via `mod tests`.
- Vue client sits in `frontend/src`; Vite tooling is defined in `frontend/package.json`.
- Operational scripts live in `scripts/` (`setup-hooks.sh`, `check-health.sh`), and deeper context sits in `specifications/` (`project_structure.md`, `development_conventions.md`).

## Build, Test, and Development Commands
- `make dev` primes Rust and Node deps, then installs git hooks.
- `make build` runs `cargo build --release`; `make frontend-build` drives the Vite production build.
- `make test` runs Rust suites; filter with `cd src-tauri && cargo test node_service`.
- UI loops and CLI spot checks: `make tauri-dev`, `make frontend-dev`, `cargo run -- --name alice --port 9000`.

## Coding Style & Naming Conventions
- Rust: `cargo fmt` (4-space indent, `snake_case` modules/tests, `UpperCamelCase` types) plus `cargo clippy -- -D warnings`.
- Frontend: `npx prettier --write src/` and `npx eslint src/ --ext .ts,.vue`.
- Use descriptive paths (`network/listener.rs`), reserve `_async` for paired sync functions, and reference `specifications/development_conventions.md` for nuances.

## Architecture & Components
- Stack: Rust 2021 on Tokio with serde/serde_json, clap, and a Tauri + Vue shell.
- `Node` manages identity, port, and peer state behind `Arc<Mutex<_>>`.
- Networking: UDP 8888 for discovery, TCP chat via `Message` (`Discovery`, `Chat`); when extending flows (`start_udp_broadcast`, `start_udp_discovery`, `connect_to_peer`, `broadcast_message`, `handle_message`), mirror the changes in specs.

## Testing Guidelines
- Integration coverage in `src-tauri/tests/` (e.g. `node_service_test.rs`); embed unit tests near the code.
- Networking changes must ship an async integration assertion proving discovery or relay behavior.
- Document executed validation commands in every PR (minimum `make test`).

## Commit & Pull Request Guidelines
- Use conventional commits (`feat:`, `fix:`, `refactor:`); scopes like `feat(network):` align with `specifications/git_standards.md`.
- Squash WIP commits and link related issues or TODO entries.
- PRs state intent, surface validation commands, and attach screenshots/logs for UX shifts.
- Request review only after `make check` or the fuller `./scripts/check-health.sh` passes.

## Security, Roadmap & Adjustments
- Run `./scripts/check-health.sh` before releases; it chains fmt, Clippy, Prettier, ESLint, tests, `cargo-audit`, `audit-ci`, and both builds.
- Triage vulnerabilities immediately or document mitigation in the PR.
- Current roadmap tracks `feature/port-handling`, `feature/cli-enhancements`, `feature/ui`, `feature/net`, `feature/notifications`.
- If plans shift, note the reason, update `specifications/task_specs/` and `specifications/TODO.md`, and capture lessons for the team.
