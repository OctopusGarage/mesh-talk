# Host architecture refactor

## Purpose

Make the desktop host and protocol runtime easier to navigate and change while
keeping Mesh Talk's current behavior, encrypted stores, wire formats, IPC command
names, and frontend data shapes stable. Pi's [minimal core and explicit host
composition](https://github.com/earendil-works/pi) are design references; Mesh Talk
remains a messenger with its existing Rust core, Tauri host, and React client.

## Current seams

- `mesh-talk-core` is already independent of Tauri and React. `Node` owns protocol
  state; `NodeRuntime` starts discovery, networking, and forwarding tasks.
- `src-tauri/src/chat_commands.rs` combines session state, IPC data models,
  messaging, channels, files, diagnostics, platform capture, and tests in about
  1,667 lines. Every command is registered through `lib.rs`.
- `frontend/src/lib/api.ts` is the typed IPC client for unrelated feature areas.
- `NodeRuntime::start` takes host configuration and five event callbacks as
  positional parameters. Callers must know their order.

## Design

### Desktop command adapter

Turn `chat_commands` into a directory with a small `mod.rs` that re-exports
the existing command functions for Tauri registration. Keep `NodeState` and
shared IPC data models in dedicated files. Group commands by responsibility:
roster/account, direct messages and reactions, channels, files/media, and
platform/diagnostics. Helpers stay next to their only caller; genuinely shared
helpers live in a small common module. Command names, arguments, serialized
fields, error mapping, and event names do not change.

The session state remains the owner of `NodeRuntime`. Commands that perform
asynchronous node work clone the `Arc<Node>` while holding the state lock, then
release the lock before awaiting. This lock-order rule is explicit in the shared
session helper and is checked by existing command and end-to-end tests.

### Runtime startup interface

Introduce a named `RuntimeConfig` for base directory, account, display name,
password, and discovery port, and a named `RuntimeEvents` bundle for callbacks.
Expose a `start_configured(config, events)` constructor. Keep `NodeRuntime::start`
as a compatibility wrapper for external SDK users; migrate in-repo host and CLI
callers to the named interface. Runtime task ownership, shutdown order, and
transport behavior remain in the core.

### Frontend IPC client

Split `frontend/src/lib/api.ts` into feature files under `lib/api/` and keep
`api.ts` as the import-compatible facade. Each feature file owns its request
construction and response types. The underlying command strings and TypeScript
exports remain unchanged.

## Order and validation

1. Record the current IPC and runtime contracts; establish baseline builds and tests.
2. Split desktop commands without changing behavior. Compile and run the Rust suite.
3. Introduce the named runtime startup interface and migrate host callers. Run
   loopback and lifecycle assertions.
4. Split the frontend IPC client. Run lint, unit tests, and production build.
5. Update architecture documentation and run `./scripts/check-health.sh`, backend
   E2E, and frontend E2E. Review the diff for accidental wire or storage changes.

## Constraints

- Preserve existing public SDK exports and Tauri command registration.
- Do not introduce plugin abstractions or new crates without a second concrete
  adapter that needs them.
- Avoid changes to crypto, sync, transport, persistence formats, and user flows.
- Changes are split into independently reviewable commits in one branch.

## Success criteria

The desktop command file becomes a small composition module; command ownership
is evident from filenames; runtime startup arguments are named; frontend IPC
wrappers are grouped by feature; all existing build, test, health, and E2E gates
pass without a compatibility change.
