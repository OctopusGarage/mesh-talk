# Mesh-Talk Domain

## Core job

Let users on the same local network chat directly, end-to-end encrypted, with no central
server. The desktop app is a Tauri (Rust) backend + React frontend; a headless CLI
(`mesh-talk-node`, with a `--post-office` relay mode) drives the same core.

## Layers (frontend → IPC → node → crypto/sync/transport → storage)

The protocol core lives in the `mesh-talk-core` crate (`crates/mesh-talk-core/src/`); the
Tauri app (`src-tauri/src/`) is a thin shell over it.

| Layer | Path | Responsibility |
|-------|------|----------------|
| **Frontend** | `frontend/src/` (React + TS) | UI; talks to the backend only via Tauri commands + events. |
| **IPC** | `src-tauri/src/{commands,chat_commands}.rs` | Auth (login/register/logout) + all messaging commands. |
| **Node** | `crates/mesh-talk-core/src/node/` | Orchestrates identity + discovery + transport + event log + DM/channel/file crypto. |
| **Crypto** | `crates/mesh-talk-core/src/{identity,transport,ratchet,channel}/`, `dm.rs` | Keys, Noise channel, Double Ratchet, sender-key group ratchet. |
| **Data/sync** | `crates/mesh-talk-core/src/{eventlog,discovery,postoffice,file}/` | Event DAG + sync, signed discovery, store-and-forward relay, file chunks. |
| **Storage** | `crates/mesh-talk-core/src/storage/` | At-rest encryption (PBKDF2 + AES-GCM) + the auth keystore. |

## Entities

| Entity | Description |
|--------|-------------|
| **Device** | A running instance: Ed25519 (sign) + X25519 (DH) identity; `user_id` = hash of its key. |
| **Account** | A user's cross-device handle (Ed25519); devices are bound to it by a certificate. |
| **Peer** | Another device found via signed UDP announce; tracked in the roster, grouped by account. |
| **Event** | Content-addressed, signed log entry (Message / React / FileManifest / MembershipChange / KeyRotation / …) in a per-conversation hash-linked DAG. |
| **Conversation** | A 1:1 DM (device-pair or account-pair) or a channel; history is the event log. |
| **Post office** | A deterministically-elected always-on peer that stores-and-forwards (still-encrypted) events for offline recipients. |

## Key invariants

- All payloads are end-to-end encrypted; the post office only ever sees ciphertext.
- Events are content-addressed + signed; ingest re-verifies hash + signature, and the log
  detects author equivocation (forks).
- DM crypto is forward-secret (Double Ratchet); channels use a per-sender sender-key ratchet
  that rotates on membership change.
- A device never registers itself as a peer (self-filter by `user_id`).

## What Mesh-Talk does NOT own

- **No central server / directory** — discovery is signed UDP multicast, scoped to the LAN.
- **No plaintext relay** — the post office forwards ciphertext only.

> Full technical detail: **[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)**.
> All documentation is indexed in the **[documentation map](docs/README.md)**.
