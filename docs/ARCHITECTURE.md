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
  Pinned connections check the responder's Noise static key after message two,
  before disclosing the initiator's static key and identity auth; the complete
  authenticated identity is still checked afterward. `accept_with_admission`
  lets an SDK host reject a verified device before sending its own identity auth.
  Noise XX still reveals the responder's static key, so this is not anonymity.
- **DM crypto** — **Double Ratchet** (`ratchet/state.rs` + `node/dm_ratchet.rs`):
  `shared_root = HKDF(DH(me,peer))`; init_alice/init_bob; DH ratchet on each inbound →
  forward secrecy + post-compromise recovery; bounded out-of-order (1000/2000); lower
  `user_id` is the canonical initiator (simultaneous-init tie-break); state encrypted on
  disk (`node/ratchet_sessions.rs`). The `dm.rs` X3DH sealed-box (no FS) is now used only
  to distribute channel keys, seal file manifests and encrypt delivery-control
  metadata (the latter has no forward secrecy and contains no message text).
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
  Profile sync queues compaction requests. `Node::run_accept_loop` owns a maintenance
  loop shared by desktop, CLI, and SDK hosts: every 3 seconds it drains at most one
  conversation on the blocking pool, with a 10-second per-conversation cooldown.
  Dropping the accept-loop future stops scheduling maintenance; an already-running
  blocking rewrite finishes atomically. The log mutex serializes rewrites with appends
  within one Node, but is not shared with a replacement Node opening the same files.
  Hosts must await consuming `NodeRuntime::stop` before reopening a runtime profile:
  it closes producer admission, aborts and joins the runtime tasks, cancels owned
  accepted connections and private-route probe children and waits for their actual
  termination, and waits for admitted
  blocking profile rewrites, delivery recovery and peer-cache writes to finish.
  Runtime Drop only requests cancellation and cannot provide that retirement barrier.
  This is in-process runtime ownership; external SDK operations and host file staging
  require the host's own lifecycle serialization, and no cross-process lease is implied.

  The desktop host serializes authentication operations separately from runtime
  replacement. Every published session has a private owner/generation lease; a
  valid startup request receives a monotonically increasing ticket under that
  session guard. Replacement awaits the old runtime's consuming stop before
  opening any new profile. Guarded startup joins all already-started initializer
  writers on errors and authorizes the synchronous producer launch, installation,
  and each inbound callback against the current lease/ticket. A late rename cannot
  change a replacement session or runtime; startup uses the current matching name.
  Successful logout forgets the original login credential before awaiting teardown.

  New `owner_*` delivery IPC captures the lease before waiting for the runtime
  lock. Local text/sticker/file enqueue keeps that lifecycle admission through
  privacy updates and staging, then holds the matching session guard over the WAL
  append. Admitted host enqueue tasks retain the lock even if their IPC caller is
  cancelled; this does not extend that guarantee to legacy IPC or arbitrary SDK
  operations. Stable file results include the original card ID and file conversation.
  Owner-sensitive identity queries atomically inspect session plus runtime; status
  queries accept at most 256 IDs and project only the exact account conversation.
  Own-device synchronization, device-addressed legacy history and incoming rows
  have no external delivery status. Existing SDK and legacy IPC response shapes
  remain available; new frontend flows must use the owner-sensitive surface.

## 4. Networking & delivery (`discovery/`, `node/`, `postoffice/`)

- **Discovery** — signed `Announce` (Ed25519, version 2) carries `x25519_pub`, `tcp_port`,
  `post_office` flag, and the `account_cert`; the device signature commits to the account
  key (prevents cert-swap). Transport is UDP multicast (`DISCOVERY_MULTICAST_GROUP`
  224.0.0.167, `DEFAULT_DISCOVERY_PORT` 47474 in `transport/net.rs`) joined on every IPv4
  interface, plus a unicast announce/response reply, a /24 unicast scan fallback, a startup
  burst, and periodic re-join. `run_broadcast` re-announces every 2 s; `run_listen` verifies
  + updates the roster; TTL eviction; `devices_of_account()` groups devices by account.
- **Legacy/channel delivery** — these paths append the sealed event, then `deliver_direct` (Noise dial +
  one sync round) and, on failure/always, `replicate_to_post_office`. Receivers run an
  accept loop (`serve_connection` → `serve_one` ingest → `emit_new_messages` decrypt/surface).
- **Post office** — deterministic election (lowest-fingerprint peer advertising
  `post_office`); `drain_from_post_office` every 3 s; relay only ever sees ciphertext.

Tracked DMs are accepted into an encrypted, bounded transaction journal and
immutable outbox before transport. Retries reuse the original logical message ID,
ratchet transition and signed device events. A receiver saves decrypted plaintext
durably before publishing an automatic encrypted delivery receipt in a separate,
domain-derived device-pair conversation. These controls never produce chat
callbacks or receipts of their own. Only a validated receipt from an exact target
device/account marks a tracked message Delivered; successful sessions, online
presence, relay custody and own-device copies leave it Awaiting.
DM file cards use the same authenticated delivery controls, without a ratchet
transition. FileManifest wire layouts remain unchanged: the receiver confirms its
exact per-device manifest event, and the sender's bounded original-event index
resolves that confirmation to the first manifest event's stable canonical card ID.
An outgoing transaction journals the immutable fanout manifests, canonical local
file row and bounded destination/scope metadata before publication. Incoming
transactions install the file row before appending their immutable receipt.
Live recovery publishes callbacks once after durable installation; startup replay
only reconstructs history and pending controls. Enqueue APIs perform local durable
acceptance only; existing send APIs additionally attempt immediate transport.

Card delivery is not file download or read confirmation. Independent immutable
chunk work survives an early card receipt and sender restart. The worker also
rotates one file destination, sends its manifest before chunks, and retires chunk
work only after the exact target confirms the final event in the validated
dense signed chunk chain. A receiver may instead confirm historical verified
completion after saving all content and reclaiming chunks: it verifies the entire
original-author chain, chunk AEAD/hashes and whole-file checksum, synchronizes the
saved file, rename and Unix parent directory, then durably journals a bounded
completion record associated with the live file card before pruning. Acceptance
reserves both completion and future local erasure metadata. Failed persistence
keeps chunks; active source fanout also keeps the original signed chunks.
Historical completion is available only to the exact authenticated origin device,
with its current full identity and certificate/account binding when present, the
current local owner, retained signed manifest/local row and current permission.
Legacy public SDK peers without account certificates retain device-only origin
binding; a later account rebind invalidates it and invisible mode cannot authorize
an account-less origin. This boolean storage proof does not mark a card Delivered.
Post offices confirm current events only; past relay custody never retires source
file work. Chunk ciphertext stays in the event log rather than being duplicated
in the journal, and staging
synchronizes the whole log at acceptance rather than synchronizing every chunk.
Existing manifests and new message/sticker events must fit both bounded single-
event sync frames before acceptance. MFM3 hash-list size is preflighted before
staging; files whose manifests cannot fit are rejected despite the nominal 4 GiB
file-size ceiling. Empty files retain their existing one-empty-chunk encoding.

File progress counts currently held chunks; historical completion does not
pretend that exported bytes remain on this node. The CLI keeps bounded pending
and recent-save queues and runs readiness checks and one export at a time on the
blocking pool. A retained managed-media copy can be exported after pruning;
every export verifies its size and checksum and synchronizes its output. Group
and own-device media use retained signed manifest/local-row references only for
local export, never for a historical network probe or a delivery receipt. Legacy
v1 exports keep their prior in-memory path and retain chunks rather than creating
new completion proofs.

Local file erasure reserves permanent bounded metadata before card acceptance.
The erased immutable manifest ID remains suppressed across restart, sync and
account adoption, without a wire tombstone or remote recall. File keys and private
scopes are rebuilt only from remaining durable aliases and validated retained
fanout references. A full erasure-metadata store refuses legacy-row deletion
before removing the row; erasure markers are never evicted to make room.
All local event allocations and own-author sync backfill first recover accepted
intents under the delivery lock, so generic reactions/manifests cannot consume
an immutable intent's reserved sequence. Failed recovery blocks competing writes.

`run_accept_loop` also owns the shared recurring delivery worker for desktop,
CLI and SDK hosts. It wakes on local acceptance and periodically retries missed
wakes, selects one destination and one receipt per iteration, and rotates logical
messages and their independent destination cursors. It takes owned bounded
snapshots after local recovery on the blocking pool, releases store locks before
network awaits, and bounds each network operation to 400 ms. Dropping the accept
loop cancels its worker and the worker's outstanding network operations. Historical DM and
control pulls use a receive-only reconciliation projection; controls can be pulled
directly or through the relay while the original sender is offline.

An optional, fixed-size exact-event storage probe follows ordinary reconciliation
on its disposable connection. New durable nodes and post offices synchronize
their store and check the current authorized projection before confirming custody.
Existing sync discriminants and ordinary frames are unchanged; legacy endpoints
reject only the optional probe, and their unqualified Have sets never retire work.
Qualified custody retires queued controls or already-confirmed unfinished fanout,
but never implies recipient delivery. Retained immutable controls remain available
to authorized pulls after a relay evicts them or disappears. New compact receipt
metadata retains the exact destination identity/account; old compact metadata
without that binding remains deduplicated but cannot be projected automatically.
Private post offices relay other-peer control pairs only with verified identities,
manual permission for both participants, participant authors and parent closure.
Local deletion/retention cancels retry metadata and disables its control projection;
it remains a local erase, not remote recall or removal of encrypted log history.
Successful cancellation invalidates subsequent guarded frame admission, including
previously snapshotted controls; it cannot retract a write already admitted.

**Invisible mode and account permissions:**
`node::PrivacyPolicy` stores a bounded, encrypted, atomically replaced local
allowlist and visibility preference. It requires one serialized owner per file
and a trusted parent directory; corrupt policy files return errors, not a public
fallback. `spawn_discovery_with_visibility` shares a `DiscoveryVisibility` gate
across startup/periodic/manual announcements, listener replies and every scan
target. Disabling waits for in-flight announcement sends; passive discovery and
multicast membership remain active, and already queued packets cannot be retracted.
Existing discovery entry points and unconfigured SDK constructors retain their
public default. Desktop startup loads the policy, verified device-signed account
announcements and routing hints before spawning network tasks. Invisible mode
suppresses local UDP presence while continuing passive discovery. Account-aware
Noise authentication checks the exact Ed25519/X25519 identity and signed presence;
a certificate alone is not an account proof. This is not traffic anonymity:
Noise XX exposes its static key and previously transmitted presence remains known.

Explicit DM/file/contact actions durably grant the verified destination account;
background profiles, file pulls, calls and relay retries never grant permission.
Manual permission publishes signed listening presence inside a pinned encrypted
connection, without requiring a message or a UDP announcement. Policy changes
invalidate existing channels; bounded per-send/handshake operation guards finish
before revocation acknowledges. The actual event log is projected separately for
each principal and conversation, including event authors and dependency closure.
Existing joined groups confer only group scope, not DM/call/pairing permissions;
file chunks inherit only a verified, authorized manifest's scope. Revocation
stops new disclosure and append but does not delete persisted local history.
Outgoing manifest scopes for every account-fanout device are recorded in a separate
encrypted `sent-manifest-scopes.log`, without duplicating the account's history entry.
Startup replays only references to matching signed manifest events and normalizes torn
trailing records before new appends. A failed append cannot install an in-memory scope;
subsequent retries repair the journal from verified local state. Restored scopes still
undergo current principal/permission checks, so restart does not bypass revocation.

Verified account proofs never store IP addresses. A separate bounded encrypted
route cache stores hints only; every reconnect pins the complete verified device
identity. A private listening-port preference lets permitted invisible peers
reconnect after both restart, subject to port availability and network changes.
Loaded routes never create online roster entries. Delivery can use eligible
verified cached routes after discovery expires; tracked, bounded authenticated
probes refresh online presence only when the pinned peer actually responds.
Failed probes leave normal offline expiry intact and never create permissions.
Conflicting passive remote device/account proofs fail closed. An explicit user
contact/grant may accept a newly device-signed account binding for the same full
Ed25519/X25519 identity, with an independent new-account permission; old account
permissions are not transferred. This invalidates existing channels. Trusted
local keystore adoption may likewise update this node's own exact binding.

In invisible mode, an elected post office has no automatic exemption: its account
must be manually permitted. Relayed DM authors must have cached verified signed
device/account proofs. A newly added device that has never been discovered or
authenticated directly cannot deliver offline through a relay; unknown proofs
are denied rather than learned from legacy relay event payloads. No new relay
proof protocol or event format is introduced.

## 5. Frontend (`frontend/`)

**React 18 + TypeScript + Tailwind + shadcn/ui**, state in **zustand**, built with Vite.
`lib/api.ts` exposes typed `auth` + `chat` wrappers over `invoke()` (every command);
`lib/events.ts` subscribes to `dm-received`/`channel-message`/`file-received`.
`store/auth.ts` holds the session; `store/chat.ts` holds per-conversation message/
reaction/unread state and routes incoming DMs to the sender's *account* (one conversation
per multi-device contact). `features/chat/` is the 3-pane app (sidebar · messages ·
members) with replies, reactions, @mentions, file send + a received-files tray, search,
and device linking; `features/auth/LoginScreen.tsx` is the only other screen.

**Contact visibility** (`store/contactPolicy.ts`, `src-tauri/src/contact_policy.rs`):
the signed-in local user's hidden account IDs and last-known names are stored separately
in `accounts/<local-user-id>/hidden-contacts.json`. Mutations validate the session owner
and replace the file atomically; this metadata is local to the device, not synced.
Settings provides a searchable management dialog and offline restoration; contact rows
also offer a hide action in their context menu, with an explicit confirmation.
Visibility filters are projections over the raw roster: contact lists, new-group invite
choices and DM search omit hidden accounts (including new devices for that account).
Raw discovery, cryptographic identity lookup, history, pins/aliases, message/file/call
receipt, notifications, and existing shared groups remain unchanged. This is **not** a
communication block or a way to become invisible to peers. A failed initial policy load
offers retry without briefly showing hidden contacts; stale responses from another login
cannot overwrite the current user's UI policy.
Atomic replacement is the save commit point. The file is synced before replacement;
directory synchronization afterward is best effort (a failure is logged without contact
data and does not falsely report that the committed change failed). A filesystem that
cannot sync directories cannot guarantee rename durability through a subsequent crash.
Search hits carry their verified account binding from the scan's own roster snapshot,
so a device leaving the live roster cannot expose a hidden account's search results.
Last-known names are display metadata: control characters are removed and names are
bounded before saving, so an abusive peer name cannot prevent hiding its account.
The hidden-contact tests also exercise actual registered Tauri command dispatch through
the headless mock runtime, including persisted hide and restoration after stopping the node.
An additional real-mesh regression keeps authenticated discovery and encrypted delivery
running while an account and its newly added device are hidden, then reopens durable history.
The `native-contact-eval` example uses the same desktop builder with isolated data/config
roots and a dev-only embedded WebDriver. The normal entry point has no driver plugin or
storage override. Three-platform native scenarios and the usability rubric are documented
in [the hidden-contact evaluation runbook](evals/hidden-contacts.md).

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
- **Local gate** (`scripts/check-health.sh`): delivery / AI regression smoke,
  fmt,
  `clippy --workspace --all-targets -D warnings`, ESLint, frontend tests (Vitest), full
  `cargo test --workspace`, typos, cargo-deny, cargo-machete, gitleaks, shellcheck, audits,
  both builds — **mirrors CI** so failures surface locally, not on CI. The `hooks/pre-commit`
  hook runs a fast slice (`check-health.sh --fast`: fmt/clippy/lint/unit only); GPG-signed
  commits are enforced by `hooks/pre-push`.
- **CI** (`.github/workflows/`): `ci.yml` (ubuntu+macOS matrix → the `verify` aggregate
  check, required by branch protection; coverage → Codecov on Linux), `check-health.yml`,
  `gitleaks.yml`, CodeQL, `dependabot-auto-merge.yml` (Dependabot PRs to `dev`, gated by required checks),
  `glib-0.20-watch.yml` (monthly dep watcher).
- **Automated bug-finding** (defence in depth — surfaces issues without anyone looking):
  - *Coverage-guided fuzzing* (`fuzz/`, `fuzz.yml`, weekly + dispatch) of every untrusted
    wire decoder; the `decoder_smoke` test is the always-on stable complement.
  - *Mutation testing* (`mutants.yml`, weekly + on PR-diff) — catches weak/missing assertions.
  - *Coverage* (Codecov), *clippy `-D warnings`*, *cargo-deny*, *cargo-machete*, *CodeQL*, *gitleaks*.
  - *Delivery / AI regression smoke* (`make eval-smoke`) keeps core smoke examples,
    prompt/agent contracts, local hooks, PR validation, and CI wiring from drifting.
    *Real model-backed AI eval* (`make ai-eval`, with `AI_EVAL_COMMAND`) scores the stable
    prompt/workflow cases. *Full real smoke* (`make smoke-full`) runs unit/integration plus
    backend and frontend E2E.
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
