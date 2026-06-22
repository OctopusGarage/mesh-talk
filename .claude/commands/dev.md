---
description: Run Mesh-Talk locally — desktop app, frontend-only, or the headless CLI
argument-hint: "[app|frontend|cli]"
allowed-tools: Bash, Read, Edit
---

Run the project locally for hands-on testing. Backend is Rust (Tauri) in
`src-tauri/`, frontend is Vue + Vite in `frontend/`.

Argument: `$ARGUMENTS` — `app` (default), `frontend`, or `cli`.

## app (default)

`make tauri-dev` — builds the frontend and launches the desktop app with the
Rust backend, hot-reloading the UI. Best for end-to-end manual testing.

## frontend

`make frontend-dev` — Vite dev server only (no backend). Use for fast UI
iteration; backend commands are stubbed/unavailable.

## cli

Headless node, no GUI — best for exercising discovery/chat between two terminals:

```bash
cargo run --bin mesh-talk-node -- --name alice
# second terminal:
cargo run --bin mesh-talk-node -- --name bob
```

> Prefix cargo with `nice -n 10` (the repo's `.claude` hook enforces this) and
> keep test parallelism low — `.cargo/config.toml` already caps it.

## Report

Say which mode you started, the port(s)/PID(s), and how to stop it. For `cli`,
confirm the two nodes discovered each other (look for the discovery log line).
