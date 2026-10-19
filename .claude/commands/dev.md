---
description: Run Mesh Talk locally — desktop app, frontend or headless CLI
argument-hint: "[app|frontend|cli]"
allowed-tools: Bash, Read, Edit
---

The Rust workspace contains the UI-free core and Tauri desktop host; the frontend
is React + TypeScript + Vite. Use the maintained commands from `Makefile` and
`docs/testing-feedback-loop.md`.

- `app` (default): `make tauri-dev` starts the desktop development flow.
- `frontend`: `make frontend-dev` starts Vite only; native IPC requires a host or
  the explicit browser-test fixture.
- `cli`: start `mesh-talk-node` with an isolated persistent profile:

```bash
nice -n 10 cargo run -p mesh-talk-core --bin mesh-talk-node -- \
  --keystore /tmp/mesh-talk-alice/device.keystore --password dev-only --name Alice
```

A second node needs another directory/identity. Production service setup and
password files are documented in `specifications/deployment_guide.md`; this is a
local development example. Do not deploy test-only cheap-KDF binaries.

Report the mode, listener/port, owned PID and stop command. Claim discovery only
after observing the peer; do not run comprehensive tests as a side effect.
