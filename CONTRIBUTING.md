# Contributing

Mesh-Talk is a serverless, end-to-end-encrypted LAN chat app: a Rust
([Tauri](https://tauri.app/)) backend in `src-tauri/` and a React + Vite frontend in
`frontend/`. A headless CLI (`mesh-talk-node`) drives the same core for testing without
the GUI. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design.

## Development setup

```bash
git clone https://github.com/OctopusGarage/mesh-talk.git
cd mesh-talk
make dev            # installs Rust + Node deps and sets up git hooks
```

Common loops:

```bash
make tauri-dev      # run the desktop app (backend + frontend) with hot reload
make frontend-dev   # frontend only (Vite dev server)
cargo run -p mesh-talk-core --bin mesh-talk-node -- \
  --keystore /tmp/alice.keystore --password pw --name alice
```

## Build, test, lint

| Command | What it does |
|---------|--------------|
| `make build` | `cargo build --release` |
| `make test` | Rust workspace test suite (`cargo test --workspace`) |
| `make e2e` | Multi-process backend E2E (real `mesh-talk-node` rigs over UDP/TCP) |
| `make lint` | `cargo clippy --workspace -- -D warnings` |
| `make format` | `cargo fmt` + `prettier` on the frontend |
| `make check` | full health check (`scripts/check-health.sh`) |

### Test layers

1. **Unit** — `cargo test -p mesh-talk-core --lib` (fast: test builds use cheap KDF
   params via `cfg(test)` / the `fast-test-kdf` feature, so the security KDF is real in
   release but instant in tests).
2. **Backend E2E** — `make e2e`: the `#[ignore]`d multi-process rigs (two-node DM,
   history-across-restart, post-office offline delivery). Fast + reliable; CI runs them in
   `e2e-backend.yml`.
3. **UI E2E** — `cd frontend && npm run e2e`: Playwright drives the real React app against
   mocked Tauri IPC and walks the full business flow. It includes large-roster stress,
   keyboard and touch checks, six-theme snapshots, and representative WCAG A/AA scans.
   Add stable `data-testid` selectors for controls exercised by browser tests. CI runs
   them in `e2e-ui.yml`.
4. **Fuzz** — `cargo check --manifest-path fuzz/Cargo.toml --bins` compiles the decoder
   targets on stable Rust. The weekly or manual Fuzz workflow runs them under nightly
   libFuzzer; see [`fuzz/README.md`](fuzz/README.md).

> **CPU note.** `.cargo/config.toml` caps `jobs`/`RUST_TEST_THREADS` so a parallel build
> doesn't saturate the machine; for ad-hoc runs prefer `cargo test -- --test-threads=2`.
> Filter while iterating: `cargo test -p mesh-talk-core node::node`.

## Coding style

- **Rust**: `cargo fmt` (4-space indent, `snake_case` modules, `UpperCamelCase`
  types) and `cargo clippy -- -D warnings`. Keep unit tests inline in a
  `#[cfg(test)] mod tests`.
- **Frontend**: `npx prettier --write src/` in `frontend/`.
- Match the surrounding code; see `specifications/development_conventions.md`.

## Editor / Claude Code feedback

Code-quality feedback in this repo is **tool-agnostic and committed**, so you get
it on clone without configuring an editor:

- A `PostToolUse` hook in `.claude/settings.json` runs `rustfmt` on Rust files
  Claude Code edits.
- CI (`.github/workflows/ci.yml`) gates formatting, Clippy (`-D warnings`),
  tests + coverage, the frontend build + ESLint, supply-chain policy
  (`cargo deny`), unused deps (`cargo machete`), spelling (`typos`), and
  shellcheck. Mutation testing (`cargo mutants`) runs in `mutants.yml`.

Quick local equivalents are wrapped in the **`/audit`** Claude command.

## Architecture

See [`CONTEXT.md`](CONTEXT.md) for the domain model and layering, and
[`AGENTS.md`](AGENTS.md) for repository conventions. In short: the frontend talks
to the backend only through Tauri `commands`; networking changes (`udp`, `tcp`,
discovery, reconnection) should mirror the docs under `specifications/`.

## Commit & pull request guidelines

- **Sign commits with GPG** (`git commit -S`). `make dev` enables `commit.gpgsign`; the
  `commit-msg` and `pre-push` hooks reject unsigned commits. No key yet?
  `gpg --full-generate-key`, then `git config --global user.signingkey <KEY_ID>`.
- Use [Conventional Commits](https://www.conventionalcommits.org/) with scopes,
  e.g. `feat(network): …`, `fix(contacts): …`. See
  `specifications/git_standards.md`.
- Squash WIP commits; link related issues.
- Run `make check` before requesting review, and state the validation commands
  you ran in the PR (minimum `make test`).
- Networking changes should ship an integration assertion proving discovery or
  relay behavior.

## Reporting issues

- 🐛 [Report a bug](https://github.com/OctopusGarage/mesh-talk/issues/new?template=bug_report.yml)
- ✨ [Request a feature](https://github.com/OctopusGarage/mesh-talk/issues/new?template=feature_request.yml)
- 🔒 Security: see [`SECURITY.md`](SECURITY.md) — do **not** open a public issue.

## Cargo build cache maintenance

`make dev` installs a daily 03:00 macOS LaunchAgent to remove dormant Mesh Talk
`target/` directories under `~/programming`. It only selects workspaces whose
`Cargo.toml` identifies the Mesh Talk repository. A target must be at least 2 GiB
and idle for seven days, or at least 40 GiB and idle for two days. The cleaner
skips all targets while Cargo, Tauri, or rustc is running. Build output is
regenerated by Cargo on the next build.

Preview with `make auto-clean-targets-dry-run`. Run the same policy immediately
with `scripts/auto-clean-targets.sh --yes`. Set `MT_TARGET_CLEAN=0` to disable
cleanup, or unload the macOS schedule with
`launchctl bootout gui/$(id -u)/org.octopusgarage.mesh-talk.target-gc`.
Set `MT_TARGET_GC_SCAN_ROOT` when installing to scan a different directory.
The existing `make clean-target-cache` still prunes older artifacts in the
current checkout during health checks.

## Sharing customization packs

See [`docs/CUSTOMIZATION_PACKS.md`](docs/CUSTOMIZATION_PACKS.md) for the avatar, theme, and sticker ZIP format, build/check commands, and the marketplace submission process. Use the [pack submission issue form](https://github.com/OctopusGarage/mesh-talk/issues/new?template=pack_submission.yml) to propose a pack; a pull request is needed to publish it in the curated catalog.
