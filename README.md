<a id="readme-top"></a>

# Mesh-Talk

[![CI](https://github.com/OctopusGarage/mesh-talk/actions/workflows/ci.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/ci.yml)
[![UI E2E](https://github.com/OctopusGarage/mesh-talk/actions/workflows/e2e-ui.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/e2e-ui.yml)
[![Backend E2E](https://github.com/OctopusGarage/mesh-talk/actions/workflows/e2e-backend.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/e2e-backend.yml)
[![Sanitizers](https://github.com/OctopusGarage/mesh-talk/actions/workflows/sanitizers.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/sanitizers.yml)
[![Fuzz](https://github.com/OctopusGarage/mesh-talk/actions/workflows/fuzz.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/fuzz.yml)
[![Mutants](https://github.com/OctopusGarage/mesh-talk/actions/workflows/mutants.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/mutants.yml)
[![Gitleaks](https://github.com/OctopusGarage/mesh-talk/actions/workflows/gitleaks.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/gitleaks.yml)
[![Scorecard](https://github.com/OctopusGarage/mesh-talk/actions/workflows/scorecard.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/scorecard.yml)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/OctopusGarage/mesh-talk/badge)](https://scorecard.dev/viewer/?uri=github.com/OctopusGarage/mesh-talk)
[![codecov](https://codecov.io/gh/OctopusGarage/mesh-talk/branch/main/graph/badge.svg)](https://codecov.io/gh/OctopusGarage/mesh-talk)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2021-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://tauri.app/)
[![React](https://img.shields.io/badge/React-18-61DAFB?logo=react&logoColor=black)](https://react.dev/)
[![platform: macOS | Windows | Linux](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-000000?logo=linux&logoColor=white)](#build-from-source)
[![DeepWiki](https://img.shields.io/badge/DeepWiki-docs-8A2BE2)](https://deepwiki.com/OctopusGarage/mesh-talk)
[![Release](https://img.shields.io/github/v/release/OctopusGarage/mesh-talk?sort=semver)](https://github.com/OctopusGarage/mesh-talk/releases)

**A serverless, end-to-end-encrypted messenger for your local network.** Mesh-Talk peers find
each other on the LAN and talk directly — no account, no cloud, no server in the middle. Your
messages and files never leave the network, and they stay encrypted the whole way.

<p align="center">
  <a href="docs/README.md"><strong>Documentation</strong></a>
  ·
  <a href="docs/ARCHITECTURE.md">Architecture</a>
  ·
  <a href="https://github.com/OctopusGarage/mesh-talk/issues/new?template=bug_report.yml">Report Bug</a>
  ·
  <a href="https://github.com/OctopusGarage/mesh-talk/issues/new?template=feature_request.yml">Request Feature</a>
</p>

<p align="center">
  <img src="docs/screenshots/hero-channel.webp" width="860"
       alt="Mesh-Talk — a group channel with a selectable brand theme, inline media, reactions, and animated stickers">
</p>

---

## Overview

Mesh-Talk is a desktop chat app ([Tauri](https://tauri.app/) v2 + React) for macOS, Windows, and
Linux, built on a UI-free Rust protocol core (`mesh-talk-core`) that also runs headless as the
`mesh-talk-node` CLI. Peers discover one another over signed UDP multicast, connect over a
Noise-encrypted TCP channel, and converge by replicating a per-conversation append-only event
log. Every message is end-to-end encrypted with forward secrecy; an optional store-and-forward
relay (the "post office") delivers messages to peers that are temporarily offline.

## Features

**Messaging**

- One-to-one DMs and group channels, addressed by identity (and fanned out across a user's linked devices)
- Reactions, @mentions, replies/threads, and full-text search over local history
- Large-file transfer (up to ~4 GiB) — chunked, encrypted per chunk, with progress and resume
- Persistent, offline-capable history; a "post office" relay delivers to peers that are away

**Security & privacy**

- End-to-end encryption with **forward secrecy** — Double Ratchet for DMs, a sender-key ratchet for channels
- Ed25519 device identities (your ID is your key fingerprint), X25519 key agreement, Noise-encrypted transport
- Safety-number contact verification with trust-on-first-use and key-change warnings
- Encrypted-at-rest keystores and message logs; no servers, no telemetry, no central account

**Network**

- Zero-config LAN discovery: signed UDP multicast announces + announce/response + a unicast subnet-scan fallback
- Dual-stack IPv4/IPv6 direct TCP; self-healing discovery that re-announces and re-joins on network changes

**Desktop experience**

- Multi-device accounts with QR/code device linking
- Live presence (online / last-seen), pinned contacts, and an in-app diagnostics panel
- Tray icon, launch-at-login, native notifications, light / dark / OLED themes, and English / 中文

## Screenshots

<table>
  <tr>
    <td width="50%" align="center">
      <img src="docs/screenshots/stickers.webp" alt="The animated-sticker panel open in a direct message">
      <br><sub><b>Built-in animated stickers</b></sub>
    </td>
    <td width="50%" align="center">
      <img src="docs/screenshots/themes.webp" alt="The theme picker with dark, light, OLED, and football-brand themes">
      <br><sub><b>Selectable themes</b></sub>
    </td>
  </tr>
  <tr>
    <td align="center">
      <img src="docs/screenshots/verify.webp" alt="The verify-contact dialog showing a safety number">
      <br><sub><b>End-to-end verification (safety number)</b></sub>
    </td>
    <td align="center">
      <img src="docs/screenshots/settings.webp" alt="The settings dialog: background behavior and privacy toggles">
      <br><sub><b>Background &amp; privacy settings</b></sub>
    </td>
  </tr>
  <tr>
    <td align="center">
      <img src="docs/screenshots/avatars-players.webp" alt="The avatar picker gallery">
      <br><sub><b>Pick an avatar</b></sub>
    </td>
    <td align="center">
      <img src="docs/screenshots/avatars-clubs.webp" alt="Group avatar crest gallery">
      <br><sub><b>Group avatars</b></sub>
    </td>
  </tr>
</table>

## How it works

1. **Discover** — each peer broadcasts a signed announce over UDP multicast (`224.0.0.167:47474`) on
   every interface; peers verify the signature and record one another. A unicast subnet scan and
   announce/response replies cover networks where multicast is flaky.
2. **Connect** — peers dial each other over TCP and run a Noise handshake for an authenticated,
   encrypted channel.
3. **Sync** — conversations are append-only event logs; peers exchange only the events the other is
   missing (bounded rounds), so history converges and survives restarts.
4. **Encrypt** — message payloads are sealed with per-conversation ratchets (Double Ratchet / sender
   key), so a compromised key can't decrypt past traffic.
5. **Relay when offline** — if a recipient is away, an elected post-office node holds the encrypted
   events and forwards them on reconnect. It never sees plaintext.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full design.

## Download & first run

Grab the `.zip` for your OS/arch from the [**Releases**](https://github.com/OctopusGarage/mesh-talk/releases)
page (e.g. `mesh-talk_vX.Y.Z_macos_arm64.zip`) and **unzip it** — inside are the installer(s) for
your platform plus a `SHA256SUMS` file. The builds are **free and unsigned** (no paid Apple/Windows
code-signing certificate), so macOS and Windows show a one-time "unidentified developer" /
SmartScreen prompt the first time — this is expected for unsigned open-source software and does
**not** mean the app is unsafe. Every release also ships a `SHA256SUMS` list, a Sigstore `cosign`
signature, and a SLSA build-provenance attestation; the **release page documents the exact verify
commands** (`shasum -c`, `cosign verify-blob`, `gh attestation verify`). How to open, per platform:

- **Linux** — no prompt at all.
  - **AppImage** (portable, no install): `chmod +x Mesh-Talk_*.AppImage && ./Mesh-Talk_*.AppImage`
  - or install the `.deb` / `.rpm` (adds an app-menu entry): `sudo dpkg -i mesh-talk_*.deb`
- **macOS** — open the `.dmg`, drag **mesh-talk** to Applications. On first launch macOS blocks an
  unsigned app, so **right-click the app → Open → Open** (only needed once). If it says
  "damaged", clear the quarantine flag with the full path (a Homebrew/Conda `xattr` in `PATH` may
  shadow the system one and lack `-r`): `/usr/bin/xattr -dr com.apple.quarantine /Applications/mesh-talk.app`.
- **Windows** — run the `.exe` (or `.msi`) installer. SmartScreen shows "Windows protected your
  PC" → click **More info → Run anyway** (only the first time). WebView2 is fetched automatically
  if missing.

After the one-time approval it behaves like any installed app. To remove the prompt entirely you'd
need paid signing (Apple Developer ID for macOS, a code-signing cert or the Microsoft Store for
Windows); Linux is always prompt-free.

> Two peers must be on the **same LAN** with the firewall allowing UDP `47474` (multicast
> `224.0.0.167`) and the app's TCP port. If they can't see each other, the in-app **Diagnostics**
> panel and the [troubleshooting guide](specifications/troubleshooting_guide.md) walk through it.

## Build from source

### Prerequisites

- **Rust** (stable, edition 2021) + Cargo
- **Node.js 20+** (frontend toolchain)
- **Tauri CLI** — `cargo install tauri-cli`
- **Linux only** — the GTK/WebKit stack:
  `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf`

### Desktop app

```bash
git clone https://github.com/OctopusGarage/mesh-talk.git
cd mesh-talk
make dev          # install Rust + Node deps + git hooks (first run is slow)
make tauri-dev    # run the app in dev mode (hot-reload frontend)
make tauri-build  # or: produce a release bundle (.app / .dmg / .exe / .deb / ...)
```

### Headless node (CLI / post-office relay)

The same core runs without a UI as `mesh-talk-node` — also the offline-delivery relay:

```bash
cargo run --bin mesh-talk-node -- --keystore alice.ks --password <pw> --name alice
```

| Flag | Meaning |
|------|---------|
| `--keystore <path>` | encrypted identity keystore, created if absent (**required**) |
| `--password <pw>` | password that encrypts the keystore (**required**) |
| `--name <name>` | display name advertised to peers (**required**) |
| `--port <n>` | TCP port to listen on (`0` = OS-assigned) |
| `--discovery-port <n>` | UDP discovery port, default `47474` (**must match across peers**) |
| `--post-office` | run as a store-and-forward relay for offline delivery (no chat REPL) |

Each instance needs its **own** keystore path — two nodes sharing one clobber each other's data.
In the chat REPL: `/msg <peer-id-prefix> <text>` to DM, `/history <peer-id-prefix>` to view a thread.

## Architecture

Two Rust crates plus a React frontend, layered so the protocol is independent of the UI:

```
mesh-talk/
├── crates/mesh-talk-core/   # UI-free protocol SDK (no Tauri dep)
│   ├── src/
│   │   ├── node/            # the serverless node: orchestration
│   │   ├── identity/  transport/  discovery/  eventlog/
│   │   ├── ratchet/  channel/  dm.rs  file/  postoffice/
│   │   ├── storage/         # encrypted append-only logs + at-rest crypto
│   │   ├── util/            # shared helpers (net, safety-number, …)
│   │   └── bin/mesh-talk-node.rs   # headless node CLI (--post-office relay mode)
│   └── tests/               # multi-process integration tests (real nodes over UDP/TCP)
├── src-tauri/               # Tauri desktop shell — a thin bridge over mesh-talk-core
├── frontend/                # React + TS + Tailwind + shadcn ("Ink & Signal" design)
├── docs/                    # architecture + documentation map
├── specifications/          # process, deployment, and convention docs
└── Makefile                 # dev/build/test/e2e/lint shortcuts
```

The frontend talks to the backend only through Tauri commands; the desktop crate holds no protocol
or crypto logic. Wire formats are versioned `bincode` framed with a magic + version byte. Full
reference: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Documentation

Everything is indexed in the **[documentation map](docs/README.md)** — start there. Highlights:

- [Architecture](docs/ARCHITECTURE.md) — components, data flow, and crypto
- [Contributing](CONTRIBUTING.md) — dev setup, commands, and conventions
- [Domain model](CONTEXT.md) and [repo conventions](AGENTS.md)
- [Deployment](specifications/deployment_guide.md) · [Troubleshooting](specifications/troubleshooting_guide.md) · [Security](SECURITY.md)

## Development

```bash
make tauri-dev    # run the desktop app (hot reload)
make test         # Rust workspace unit tests
make e2e          # multi-process backend E2E (real nodes)
make lint fix     # clippy -D warnings · auto-fix + format
make check        # the full local health gate
```

The UI flow is covered by a Playwright suite (`cd frontend && npm run e2e`). Quality is enforced in
CI across macOS/Windows/Linux — formatting, Clippy (`-D warnings`), tests + coverage, the frontend
build + ESLint, supply-chain policy (`cargo deny`), unused-dep and spelling checks — plus fuzzing,
mutation testing, secret scanning, and OpenSSF Scorecard. Commits are GPG-signed (the hooks set up
by `make dev` enforce it). See [CONTRIBUTING.md](CONTRIBUTING.md) for the full workflow.

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) for the dev setup,
build/test/lint commands, and conventions, and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for the
community guidelines.

## Security

Mesh-Talk is end-to-end encrypted with forward secrecy and has no servers or telemetry. Please
report vulnerabilities privately — see [SECURITY.md](SECURITY.md). Do not open a public issue for
security reports.

## License

Open source under the [MIT License](LICENSE).

<p align="right">(<a href="#readme-top">back to top</a>)</p>
