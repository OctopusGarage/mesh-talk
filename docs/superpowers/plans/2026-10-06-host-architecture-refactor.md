# Host Architecture Refactor Implementation Plan

> **For agentic workers:** Execute these tasks in order and verify each checkpoint before proceeding.

**Goal:** Make the desktop host and runtime interfaces easier to navigate without changing the protocol, IPC contract, or user behavior.

**Architecture:** Keep the existing UI-free core and Tauri shell. Organize IPC commands by feature, give runtime startup named configuration and event inputs, and group frontend IPC wrappers behind the existing `api.ts` import path.

**Tech Stack:** Rust 2021, Tokio, Tauri 2, React, TypeScript, Vite.

---

## Baseline

- [x] Build `frontend/dist` with `cd frontend && npm run build` (Tauri compile prerequisite).
- [x] Run `cargo test --workspace` successfully before moving code.

## Task 1: Desktop command modules

**Files:** `src-tauri/src/chat_commands.rs`, new `src-tauri/src/chat_commands/{state,models,account,messaging,channels,files,platform,diagnostics}.rs`, `src-tauri/src/lib.rs`.

- [ ] Move `NodeState` into `state.rs` and serializable IPC types into `models.rs`.
- [ ] Move each command and its sole-use helpers into the matching feature file. Keep shared ID parsers and reaction conversion in `mod.rs` until their users are clear.
- [ ] Keep `lib.rs` registration through the existing `crate::chat_commands::<name>` paths by re-exporting each command from `mod.rs`.
- [ ] Preserve `#[tauri::command]` on every moved function and retain all argument names and result types. For example:

```rust
pub use messaging::{history, react_dm, send_dm};
pub use channels::{channel_history, create_channel, list_channels};
pub use state::NodeState;
```

- [ ] Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`. Repair imports and visibility without changing command behavior.
- [ ] Compare the registration list before and after with `git diff -- src-tauri/src/lib.rs` and inspect the generated Tauri command tests.
- [ ] Commit `refactor(tauri): group chat commands by feature`.

## Task 2: Named runtime startup

**Files:** `crates/mesh-talk-core/src/node/runtime.rs`, `crates/mesh-talk-core/src/node/mod.rs`, `src-tauri/src/commands.rs`, `crates/mesh-talk-core/src/bin/mesh-talk-node.rs`, runtime tests.

- [ ] Add a `RuntimeConfig` struct with `base_dir: PathBuf`, `account_id: String`, `display_name: String`, `password: String`, and `discovery_port: u16`.
- [ ] Add a `RuntimeEvents` struct with named boxed `Fn` callbacks for DM, channel, file, profile, and call signals. Its `new` constructor accepts the same callbacks as the existing start method in the same order.
- [ ] Add `NodeRuntime::start_configured(config, events)` and move the startup body there. Keep `NodeRuntime::start` as an adapter to preserve the public SDK contract:

```rust
pub async fn start_configured(
    config: RuntimeConfig,
    events: RuntimeEvents,
) -> Result<NodeRuntime, RuntimeError>;
```

- [ ] Re-export the new types from `node/mod.rs`; migrate desktop and CLI startup call sites to the named constructor.
- [ ] Run targeted runtime and loopback tests, then `cargo test --workspace` and Clippy.
- [ ] Commit `refactor(core): name runtime startup inputs`.

## Task 3: Frontend IPC modules

**Files:** `frontend/src/lib/api.ts`, new `frontend/src/lib/api/{auth,chat,calls,diagnostics,contact-policy,favorites,avatars,settings}.ts` and shared types as needed.

- [ ] Move existing wrapper objects by feature, keeping every `invoke` command string and payload key unchanged.
- [ ] Keep `frontend/src/lib/api.ts` as a facade with the original named exports:

```ts
export { auth } from "./api/auth";
export { chat } from "./api/chat";
export { calls } from "./api/calls";
```

- [ ] Run `npm run lint`, `npm test -- --run`, and `npm run build` in `frontend/`.
- [ ] Compare old and new exports and command strings; commit `refactor(frontend): group IPC client by feature`.

## Task 4: Architecture and full verification

**Files:** `docs/ARCHITECTURE.md`, `docs/README.md` if a documentation entry is required.

- [ ] Update the process diagram and module descriptions to match the new host interfaces.
- [ ] Run `./scripts/check-health.sh`, `make e2e`, and `cd frontend && npm run e2e`.
- [ ] Inspect `git diff origin/dev...HEAD` for accidental changes to event codecs, crypto, storage, command registration, or serialized field names.
- [ ] Record validation and any pre-existing environmental failures in the PR description. Commit documentation changes and open a draft PR for review.
