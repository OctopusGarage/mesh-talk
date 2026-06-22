# Mesh-Talk Architecture

A decentralized, end-to-end-encrypted LAN messenger. **Tauri** desktop shell, **Rust**
backend, **React + TypeScript** frontend. No server: peers discover each other over signed UDP
**multicast** (group `224.0.0.167`, port 47474), connect directly over a Noise-encrypted TCP
channel, and store messages as an append-only, hash-linked **event log** that syncs CRDT-style.
When a peer is offline, an elected **post office** node stores-and-forwards the (still-encrypted)
events.

> The earlier RSA-contact / plaintext-UDP / TCP-relay *legacy* stack has been retired;
> this serverless stack is the entire product and lives at `/`. The only retained
> piece outside the node is the auth/session layer (`services/auth_service.rs` + `state.rs`),
> which `login` uses before starting the node.

---

## 1. Process & layers

```
React UI (features/chat/*.tsx) ──invoke()──▶ Tauri IPC (chat_commands.rs)
        ▲   ──listen() events──                         │
        │                                               ▼
        │                                   NodeRuntime (node/runtime.rs)
        │                                   starts 7 bg tasks per login:
        │                                   UDP listen / UDP multicast announce /
        └────────── on_dm/on_channel/on_file callbacks ── TCP accept / PO drain /
                                                          DM·channel·file forwarders
                                                               │
                                                          Node (node/node.rs) — orchestration
        ┌──────────────┬───────────────┬──────────────┬───────┴──────┐
   identity/      transport/        eventlog/       discovery/    postoffice/
   ratchet/       (Noise XX)        (DAG + sync)    (signed UDP)  (elect+relay)
   channel/ dm/
        └────────────── storage/encryption.rs (PBKDF2-600k + AES-256-GCM at rest) ──┘
```

## 2. Crypto & identity (`identity/`, `transport/`, `ratchet/`, `dm/`, `channel/`)

- **DeviceIdentity** — per device: Ed25519 (sign) + X25519 (DH). `user_id =
  SHA-256("mesh-talk-id-v1" ‖ ed25519_pub)[:16]` (32 hex).
- **Account** (multi-device) — cross-device Ed25519. `account_id =
  SHA-256("mesh-talk-account-v1" ‖ pub)[:16]`. A **DeviceCertificate** is the account
  key's signature over a device key (domain `mesh-talk-device-cert-v1`), binding device→account.
- **At rest** (`storage/encryption.rs`) — `salt(16) ‖ nonce(12) ‖ AES-256-GCM(secret)`,
  key via **PBKDF2-HMAC-SHA256, 600k rounds**. Used by every store (device/account
  keystore, event log, sent/received logs, ratchet sessions, channel senders, post office).
- **Transport** (`transport/`, snow) — **Noise_XX_25519_ChaChaPoly_BLAKE2s** with a
  post-handshake identity exchange: each side signs `"mesh-talk-transport-auth-v1" ‖
  handshake_hash` and verifies the advertised X25519 == the Noise-authenticated static key
  → binds the Ed25519 identity to the channel. 4-byte length framing, MAX_FRAME 65535.
- **DM crypto** — **Double Ratchet** (`ratchet/state.rs` + `node/dm_ratchet.rs`):
  `shared_root = HKDF(DH(me,peer))`; init_alice/init_bob; DH ratchet on each inbound →
  forward secrecy + post-compromise recovery; bounded out-of-order (1000/2000); lower
  `user_id` is the canonical initiator (simultaneous-init tie-break); state encrypted on
  disk (`node/ratchet_sessions.rs`). The `dm.rs` X3DH sealed-box (no FS) is now used only
  to distribute channel keys and seal file manifests.
- **Channels** — per-sender **sender-key** group ratchet (`channel/sender_key.rs`):
  single-use message keys; membership add/remove rotates the **epoch** and re-distributes
  sender-key distributions (sealed per member via the DM sealed-box). Sender chains are
  persisted so a restarted node can resume sending.
- **Multi-device** — `DmEnvelope{route, msg_id, body}` (magic `MTDE1`) carries
  sender/recipient *account* routing inside the ciphertext; `send_to_account` fans a
  per-device ratcheted copy to every device of the target account + self-syncs to own
  devices; `account_history` merges by `msg_id`. Device **linking** (`node/pairing.rs`):
  one-time 128-bit code (SHA-256 authenticator binding both device keys, constant-time
  check) → account secret + cert + history backfill transferred over the Noise channel.

## 3. Event log & sync (`eventlog/`)

- **Event** — content-addressed: `id = SHA-256(domain ‖ content)`; fields `conversation,
  author (Ed25519 pub, self-certifying), seq, parents (→ hash-linked DAG), lamport,
  wall_clock, kind, ciphertext, sig`. `EventKind`: Message/Edit/Delete/React/ReadMarker/
  MembershipChange/KeyRotation/FileManifest (indices frozen for wire stability).
- **EventLog** (in-memory, `store.rs`) — per-conversation DAG; tracks `heads` (frontier),
  `version` (per-author max seq → equivocation/fork detection), `max_lamport`. `events()`
  returns `(lamport, id)`-sorted (deterministic topological order).
- **Sync** (`sync.rs`) — three-message id-set reconciliation: `Request(have)` →
  `Response(missing + responder have)` → `Followup(what responder lacks)`, bounded by
  frame budget (`MAX_PLAINTEXT`), multiple rounds (≤ `MAX_SYNC_ROUNDS`). Every event is
  re-verified (hash + signature) on ingest.
- **Persistence** (`persist.rs`) — `MTLOG1 ‖ salt ‖ [len ‖ nonce ‖ AES-256-GCM(event)]…`,
  append-only; a torn trailing record (crash mid-write) is dropped on reload and re-synced.

## 4. Networking & delivery (`discovery/`, `node/`, `postoffice/`)

- **Discovery** — signed `Announce` (Ed25519, version 2) carries `x25519_pub`, `tcp_port`,
  `post_office` flag, and the `account_cert`; the device signature commits to the account
  key (prevents cert-swap). Transport is UDP multicast (`DISCOVERY_MULTICAST_GROUP`
  224.0.0.167, `DEFAULT_DISCOVERY_PORT` 47474 in `transport/net.rs`) joined on every IPv4
  interface, plus a unicast announce/response reply, a /24 unicast scan fallback, a startup
  burst, and periodic re-join. `run_broadcast` re-announces every 2 s; `run_listen` verifies
  + updates the roster; TTL eviction; `devices_of_account()` groups devices by account.
- **Delivery** — `send_*` appends the sealed event, then `deliver_direct` (Noise dial +
  one sync round) and, on failure/always, `replicate_to_post_office`. Receivers run an
  accept loop (`serve_connection` → `serve_one` ingest → `emit_new_messages` decrypt/surface).
- **Post office** — deterministic election (lowest-fingerprint peer advertising
  `post_office`); `drain_from_post_office` every 3 s; relay only ever sees ciphertext.

## 5. Frontend (`frontend/`)

**React 18 + TypeScript + Tailwind + shadcn/ui**, state in **zustand**, built with Vite.
`lib/api.ts` exposes typed `auth` + `chat` wrappers over `invoke()` (every command);
`lib/events.ts` subscribes to `dm-received`/`channel-message`/`file-received`.
`store/auth.ts` holds the session; `store/chat.ts` holds per-conversation message/
reaction/unread state and routes incoming DMs to the sender's *account* (one conversation
per multi-device contact). `features/chat/` is the 3-pane app (sidebar · messages ·
members) with replies, reactions, @mentions, file send + a received-files tray, search,
and device linking; `features/auth/LoginScreen.tsx` is the only other screen.

## 6. Binaries

- `mesh-talk` (`main.rs` → `lib.rs::run_tauri`) — the desktop app.
- `mesh-talk-node` (`bin/mesh-talk-node.rs`) — headless node CLI; `--post-office`
  runs relay mode.

## 7. Build, test, CI

- **Workspace** (two layered crates): `crates/mesh-talk-core/` — the UI-free protocol
  core / SDK foundation (lib `mesh_talk_core` + the `mesh-talk-node` CLI bin) — and
  `src-tauri/` — the Tauri desktop shell, a thin layer that depends on the core. Shared
  dependency versions live in the root `[workspace.dependencies]`. The app references the
  core as `mesh_talk_core::…`; a third party can depend on `mesh-talk-core` alone (no Tauri).
- **Local gate** (`scripts/check-health.sh`): fmt,
  `clippy --workspace --all-targets -D warnings`, ESLint, frontend tests (Vitest), full
  `cargo test --workspace`, typos, cargo-deny, cargo-machete, gitleaks, shellcheck, audits,
  both builds — **mirrors CI** so failures surface locally, not on CI. The `hooks/pre-commit`
  hook runs a fast slice (`check-health.sh --fast`: fmt/clippy/lint/unit only); GPG-signed
  commits are enforced by `hooks/pre-push`.
- **CI** (`.github/workflows/`): `ci.yml` (ubuntu+macOS matrix → the `verify` aggregate
  check, required by branch protection; coverage → Codecov on Linux), `check-health.yml`,
  `gitleaks.yml`, CodeQL, `dependabot-auto-merge.yml` (patch-only),
  `glib-0.20-watch.yml` (monthly dep watcher).
- **Automated bug-finding** (defence in depth — surfaces issues without anyone looking):
  - *Coverage-guided fuzzing* (`fuzz/`, `fuzz.yml`, weekly + dispatch) of every untrusted
    wire decoder; the `decoder_smoke` test is the always-on stable complement.
  - *Mutation testing* (`mutants.yml`, weekly + on PR-diff) — catches weak/missing assertions.
  - *Coverage* (Codecov), *clippy `-D warnings`*, *cargo-deny*, *cargo-machete*, *CodeQL*, *gitleaks*.
  - *Claude operations* (`.claude/`): `/bug-hunt` fans out read-only `bug-hunter` subagents
    (adversarial audit, verified findings only) over the core; `code-reviewer` reviews a diff;
    `/e2e` runs the `e2e-runner` over the real multi-process integration suite.
- **Crypto primitives**: ed25519-dalek, x25519-dalek (need rand_core 0.6 — keep `rand` at
  0.8), aes-gcm, snow (Noise), sha2, hkdf, pbkdf2, bincode.

## 8. Security posture (summary)

Standard primitives + consistent domain separation; AEAD everywhere; PBKDF2-600k at rest;
Noise XX with identity binding; Double-Ratchet + sender-key forward secrecy with
zeroize-on-drop; signed content-addressed log with fork detection; deterministic relay
election; the sync `have` id-set is streamed in chunks so arbitrarily large conversations
reconcile; the relay bounds its storage (LRU whole-conversation eviction) and its serve
loop (round cap + idle timeout). Known limitations (by design): device linking relies on
the one-time pairing code (no separate SAS UX); backfill history travels as plaintext over
the Noise channel; a relay inevitably sees event authors + the participant pair (content
stays encrypted).
