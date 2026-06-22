# Project Context for `mesh-talk`

This is the main context file for AI tools. For the authoritative technical reference see
**[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)**; for the domain model see
**[`CONTEXT.md`](CONTEXT.md)**. The `specifications/` directory holds the project overview
and the process/convention docs linked below.

## Project Overview

Mesh-Talk is a **serverless, end-to-end-encrypted desktop chat app** (Rust + Tauri 2 +
React). Peers on the same LAN discover each other over Ed25519-signed UDP multicast announces,
connect directly over a Noise-encrypted channel, and store messages as an append-only,
hash-linked event log that syncs CRDT-style. An elected "post office" peer stores-and-
forwards (still-encrypted) events for offline recipients.

### Technology Stack
- **Language**: Rust 2021 · **Desktop**: Tauri 2.x · **Frontend**: React + TypeScript + Tailwind + shadcn/ui (Vite)
- **Async**: Tokio · **Serialization**: bincode (wire/at-rest), serde_json (IPC)
- **Crypto**: Ed25519 + X25519 (dalek; keeps `rand` 0.8), Noise_XX (snow), Double Ratchet
  + sender-key group ratchet, AES-256-GCM + ChaChaPoly, SHA-256/HKDF, PBKDF2-600k at rest
- **CLI**: clap (the `mesh-talk-node` headless / `--post-office` relay binary)

### Features
1:1 DMs + group channels with forward secrecy, file sharing, reactions, replies/threads,
@mentions, search, multi-device (one account across devices via device linking +
account-addressed fan-out), system-tray integration, encrypted local storage.

## AI Tool Compatibility

This spec is designed to be usable by AI coding assistants (Claude Code, Codex, Gemini
CLI, Copilot, etc.). Conventions for agents live in [`AGENTS.md`](AGENTS.md).

## Key Documentation

- Architecture (authoritative): [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- Domain model: [`CONTEXT.md`](CONTEXT.md)
- Design specs + implementation plans: `docs/superpowers/specs/`, `docs/superpowers/plans/`
- Development conventions: [`specifications/development_conventions.md`](specifications/development_conventions.md)
- Project structure + tech stack: see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) (and the Tech Stack section above)
- Git standards: [`specifications/git_standards.md`](specifications/git_standards.md)
- Task completion criteria: [`specifications/task_completion_criteria.md`](specifications/task_completion_criteria.md)
- Status: [`specifications/TODO.md`](specifications/TODO.md)

## Important Considerations

1. **Forward secrecy** — DMs use a Double Ratchet; channels use a sender-key ratchet that
   rotates on membership change. Don't weaken these when extending messaging.
2. **The post office only sees ciphertext** — never route plaintext through a relay.
3. **Offline delivery** depends on the post office or the recipient coming back online and
   syncing; there is no central store.
4. **`rand` stays at 0.8** — the dalek crates require `rand_core` 0.6; bumping breaks the build.
5. **Dependencies** — `cargo-deny` + `cargo-audit` run in the gate; keep advisories clean.
6. **Known gaps** — device linking has no SAS/key-pinning (LAN MITM window); backfill
   history travels as plaintext over the Noise channel.

## Plan Adjustment Guidance

If an implementation plan proves unsuitable during development:

1. **Assess** — document why the current plan is not working.
2. **Adjust** — modify the approach based on what was learned.
3. **Update docs** — revise `docs/superpowers/` and `docs/ARCHITECTURE.md` to match.
4. **Update status** — reflect scope changes in `specifications/TODO.md`.
5. **Continue** — proceed with the updated plan.
6. **Capture lessons** — record insights for future reference.
