# Mesh-Talk Domain

## Core job

Let users on the same local network chat directly, end-to-end encrypted, without
a vendor-operated server. On existing routed networks, peers can opt into routed
discovery or a post-office bridge for cross-site introductions and encrypted
delivery. The desktop app is a Tauri (Rust) backend + React frontend; a headless
CLI (`mesh-talk-node`, with a `--post-office` relay mode) drives the same core.

## Layers (frontend → IPC → node → crypto/sync/transport → storage)

The protocol core lives in the `mesh-talk-core` crate (`crates/mesh-talk-core/src/`); the
Tauri app (`src-tauri/src/`) is a thin shell over it.

| Layer         | Path                                                                       | Responsibility                                                                      |
| ------------- | -------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| **Frontend**  | `frontend/src/` (React + TS)                                               | UI; talks to the backend only via Tauri commands + events.                          |
| **IPC**       | `src-tauri/src/commands.rs`, `src-tauri/src/chat_commands/`                | Auth (login/register/logout) + messaging commands.                                  |
| **Node**      | `crates/mesh-talk-core/src/node/`                                          | Orchestrates identity + discovery + transport + event log + DM/channel/file crypto. |
| **Crypto**    | `crates/mesh-talk-core/src/{identity,transport,ratchet,channel}/`, `dm.rs` | Keys, Noise channel, Double Ratchet, sender-key group ratchet.                      |
| **Data/sync** | `crates/mesh-talk-core/src/{eventlog,discovery,postoffice,file}/`          | Event DAG + sync, signed discovery, store-and-forward relay, file chunks.           |
| **Storage**   | `crates/mesh-talk-core/src/storage/`                                       | At-rest encryption (PBKDF2 + AES-GCM) + the auth keystore.                          |

## Entities

| Entity               | Description                                                                                                                                                  |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Device**           | A running instance: Ed25519 (sign) + X25519 (DH) identity; `user_id` = hash of its key.                                                                      |
| **Account**          | A user's cross-device handle (Ed25519); devices are bound to it by a certificate.                                                                            |
| **Peer**             | Another device learned through verified LAN/routed discovery or eligible relay introduction; tracked by device identity and grouped by account.              |
| **Event**            | Content-addressed, signed log entry (Message / React / FileManifest / MembershipChange / KeyRotation / …) in a per-conversation hash-linked DAG.             |
| **Conversation**     | A 1:1 DM (device-pair or account-pair) or a channel; history is the event log.                                                                               |
| **Outgoing intent**  | A user's attempt to send text, a sticker, or a file to a Conversation. It can be pending or failed before acceptance assigns a stable Event ID.              |
| **Post office**      | An equal relay that temporarily holds opaque recipient-encrypted events; bridge mode adds opted-in presence and explicitly trusted federation.               |
| **Recovery archive** | A user-created encrypted snapshot (`.mtbackup`) unlocked with a separate recovery key; restore keeps the account identity but creates a new device identity. |

The [network architecture baseline](docs/network-design-reference.md) defines route
provenance, direct reachability, per-device relay permissions, custody and exact
recipient confirmation. These are separate concepts; UI/IPC consume core policy
rather than infer reachability or human identity from an address/name.

## Key invariants

- Message payloads are end-to-end encrypted; the post office sees ciphertext and
  routing metadata, as described in [`docs/post-office-metadata-privacy.md`](docs/post-office-metadata-privacy.md).
- Events are content-addressed + signed; ingest re-verifies hash + signature, and the log
  detects author equivocation (forks).
- DM crypto is forward-secret (Double Ratchet); channels use a per-sender sender-key ratchet
  that rotates on membership change.
- A device never registers itself as a peer (self-filter by `user_id`).
- Post offices remain infrastructure, excluded from person/group pickers.
- Relay custody is not recipient confirmation; a receipt retires only its exact destination.
- Public presence opt-in is independent of private/group authorization and relay data directions.
- Restoring an archive never copies old device keys or ratchets and does not revoke an old device.

## What Mesh-Talk does NOT own

- **No vendor-operated central server / directory** — LAN discovery is signed
  UDP multicast; optional routed discovery and peer-operated bridge directories
  require existing network reachability and explicit opt-in.
- **No plaintext relay** — the post office forwards ciphertext only.

> Full technical detail: **[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)**.
> All documentation is indexed in the **[documentation map](docs/README.md)**.
