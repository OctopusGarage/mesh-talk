# Mesh-Talk

[![CI](https://github.com/OctopusGarage/mesh-talk/actions/workflows/ci.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/ci.yml)
[![UI E2E](https://github.com/OctopusGarage/mesh-talk/actions/workflows/e2e-ui.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/e2e-ui.yml)
[![Backend E2E](https://github.com/OctopusGarage/mesh-talk/actions/workflows/e2e-backend.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/e2e-backend.yml)
[![Sanitizers](https://github.com/OctopusGarage/mesh-talk/actions/workflows/sanitizers.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/sanitizers.yml)
[![Fuzz](https://github.com/OctopusGarage/mesh-talk/actions/workflows/fuzz.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/fuzz.yml)
[![Mutation sample](https://github.com/OctopusGarage/mesh-talk/actions/workflows/mutants.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/mutants.yml)
[![Gitleaks](https://github.com/OctopusGarage/mesh-talk/actions/workflows/gitleaks.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/gitleaks.yml)
[![Scorecard](https://github.com/OctopusGarage/mesh-talk/actions/workflows/scorecard.yml/badge.svg)](https://github.com/OctopusGarage/mesh-talk/actions/workflows/scorecard.yml)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/OctopusGarage/mesh-talk/badge)](https://scorecard.dev/viewer/?uri=github.com/OctopusGarage/mesh-talk)
[![codecov](https://codecov.io/gh/OctopusGarage/mesh-talk/branch/main/graph/badge.svg)](https://codecov.io/gh/OctopusGarage/mesh-talk)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://tauri.app/)
[![React](https://img.shields.io/badge/React-19-61DAFB?logo=react&logoColor=black)](https://react.dev/)
[![platform: macOS | Windows | Linux](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-000000?logo=linux&logoColor=white)](#build-from-source)
[![DeepWiki](https://img.shields.io/badge/DeepWiki-docs-8A2BE2)](https://deepwiki.com/OctopusGarage/mesh-talk)
[![Release](https://img.shields.io/github/v/release/OctopusGarage/mesh-talk?sort=semver)](https://github.com/OctopusGarage/mesh-talk/releases)

**A private messenger built for the networks you already own.** Mesh-Talk is a desktop app
for direct messages, group channels, and file sharing. Peers discover one another on a
local network and communicate without a cloud account or vendor-operated server. When
people are farther apart or offline, optional routed discovery and peer-operated post
offices extend delivery without changing who can read the content.

<p align="center">
  <img src="docs/screenshots/hero-barcelona.webp" width="860"
       alt="Mesh-Talk desktop app showing a group conversation, contact list, and message composer">
</p>
<p align="center"><sub>Desktop interface shown with deterministic demo identities and messages.</sub></p>

## Conversations that stay with you

- **Talk naturally.** Send one-to-one messages or create group channels. Replies, reactions,
  mentions, search, presence, and local history keep conversations usable as they grow.
- **Share more than text.** Transfer files in encrypted chunks; send images and stickers;
  personalize the app with themes, avatars, and installable customization packs.
- **Keep your account portable.** Link devices to the same account and make a manual,
  encrypted backup with a separate recovery key. Conversation events can sync from other
  online channel members as well as from a post office when that path is available.
- **Choose your visibility.** Verify contacts by safety number, use privacy presets or
  invisible mode, recall messages, and set a fixed expiry time for an outgoing message.
  The expiry starts when you send it; the sender's copy stays in history. Updated recipient
  devices remove their local copies at the deadline.

<table>
  <tr>
    <td><img src="docs/screenshots/verify.webp" alt="Contact identity verification view" width="420"></td>
    <td><img src="docs/screenshots/themes.webp" alt="Desktop theme selection" width="420"></td>
  </tr>
  <tr>
    <td align="center"><sub>Verify a contact's identity</sub></td>
    <td align="center"><sub>Make the desktop app your own</sub></td>
  </tr>
</table>

## How messages travel

1. **Discover.** Devices announce signed identities over UDP multicast on the LAN. A
   manually supplied address or an existing routed network can introduce a distant peer.
2. **Connect.** Peers authenticate an encrypted Noise connection. Direct messages use a
   Double Ratchet; group channels use per-sender key ratchets.
3. **Sync.** Devices exchange bounded, content-addressed conversation events. A device can
   catch up after reconnecting, including through other online members of a channel.
4. **Deliver later.** If the recipient is offline, an available post office can hold
   recipient-encrypted events until they return. An opt-in bridge can introduce peers
   across sites, using existing network routes.

| Connection        | What happens                                              | Practical limit                                                  |
| ----------------- | --------------------------------------------------------- | ---------------------------------------------------------------- |
| Same LAN          | Automatic discovery and a direct connection               | Multicast and firewall rules must allow the peers to meet.       |
| Different LANs    | Existing routed discovery or an opt-in post-office bridge | Mesh-Talk does not create a VPN or join IP subnets.              |
| Recipient offline | A post office stores encrypted events for later delivery  | Mailboxes are capacity-limited and expire after at most 30 days. |

The post office cannot read message content, but it can observe routing metadata. It is
an optional delivery path; equal trusted relays can exchange signed presence and encrypted
mailboxes over reachable paths. The
[network topology guide](docs/network-topologies.md) explains which paths support
messages, files, and calls. The [architecture reference](docs/ARCHITECTURE.md) describes
the protocol and its trust boundaries in detail. The
[network architecture baseline](docs/network-design-reference.md) brings together the
communication model, module responsibilities, settings and illustrated scenarios.

## Security and privacy

Each device has its own Ed25519 signing identity and X25519 key agreement identity.
Transport is authenticated with Noise; direct messages use the Double Ratchet, while
channels rotate sender-key epochs as membership changes. Local stores are encrypted at
rest. Compare safety numbers with contacts when identity assurance matters.

Encryption does not hide every connection or routing detail. Expiring messages are a
local retention control: recipients can copy content or modify their clients, and older
clients do not enforce the expiry frame. For the exact limits, see the
[security posture](docs/ARCHITECTURE.md#8-security-posture-summary) and
[post-office metadata guide](docs/post-office-metadata-privacy.md).

## Install and start talking

1. Download the archive for your OS and CPU from
   [GitHub Releases](https://github.com/OctopusGarage/mesh-talk/releases). The default
   edition bundles optional visual packs; the lite edition omits those bundles and
   animated sticker assets. Both can install packs later.
2. Unzip the archive and install the app using the platform package below.
3. Create a local account on each person's device. On the same LAN, peers should appear
   through automatic discovery. Verify a contact's safety number before relying on that
   identity. If a peer does not appear, open **Connection → Diagnostics**.

| Platform | Install                                                            |
| -------- | ------------------------------------------------------------------ |
| macOS    | Open the DMG and drag Mesh-Talk to Applications.                   |
| Windows  | Run the EXE installer, or use the MSI for managed installation.    |
| Linux    | Install the DEB or RPM for your distribution, or run the AppImage. |

Release archives include checksums, signatures, and build provenance instructions on the
release page. Current macOS and Windows installers do not carry commercial OS signing,
so first launch may need the platform's usual downloaded-app confirmation. The app's
**About → Check for updates** action checks only when requested; it downloads a verified
archive but leaves installation to you. See the [install and update guide](docs/updates.md)
for platform steps and the [troubleshooting guide](specifications/troubleshooting_guide.md)
for connection issues.

### Build from source

Install stable Rust, Node.js 22+, and the [Tauri prerequisites](specifications/deployment_guide.md)
for your platform, then:

```bash
git clone https://github.com/OctopusGarage/mesh-talk.git
cd mesh-talk
make dev
make tauri-dev
```

`make tauri-build` packages the desktop app. The UI-free Rust core also runs as a
headless node, with a separate keystore for each instance:

```bash
cargo run -p mesh-talk-core --bin mesh-talk-node -- \
  --keystore ./alice.keystore --password-file ./alice.password --name Alice
```

Protect the password file; on Unix, use mode `0600`. For post-office flags, service
deployment, and release builds, follow the
[deployment guide](specifications/deployment_guide.md).

Bots and local AI agents can send through scoped credentials. See the
[bot integration guide](docs/bot-integrations.md) for desktop and headless setup.

## Develop and contribute

| Directory                        | What lives there                                                                    |
| -------------------------------- | ----------------------------------------------------------------------------------- |
| `crates/mesh-talk-core/`         | Identity, transport, discovery, event sync, messaging, files, post office, and CLI. |
| `crates/mesh-talk-integrations/` | Scoped local messaging service and dedicated bot/admin CLI.                         |
| `crates/mesh-talk-mcp/`          | Local MCP adapter for agent messaging tools.                                        |
| `src-tauri/`                     | Desktop shell, session state, and IPC commands.                                     |
| `frontend/`                      | React interface and Playwright UI tests.                                            |
| `docs/` and `specifications/`    | Architecture, operation guides, test evidence, and conventions.                     |

The core is independent of the desktop UI. Start with an affected test during development,
then run the relevant end-to-end layer and the full gate before a PR:

```bash
make test                        # Rust workspace tests
make e2e                         # Multi-process backend scenarios
(cd frontend && npm run e2e)     # Playwright desktop UI scenarios
./scripts/check-health.sh        # CI-equivalent health gate
```

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution workflow. The
[documentation map](docs/README.md) points to deeper feature and operations guides;
the [testing feedback loop](docs/testing-feedback-loop.md) helps select focused tests.

## Community

Report vulnerabilities privately through [SECURITY.md](SECURITY.md). Contributions follow
the [code of conduct](CODE_OF_CONDUCT.md). Mesh-Talk is released under the
[MIT License](LICENSE).
