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
React UI ── lib/api/*.ts ──invoke()──▶ Tauri IPC (chat_commands/)
   ▲                                    │
   │ Tauri events                       ▼
   └─────────────── RuntimeEvents ← NodeRuntime (node/runtime.rs)
                                           │ discovery, accept, maintenance
                                           ▼
                                   Node (node/node.rs) — orchestration
        ┌──────────────┬───────────────┬──────────────┬───────┴──────┐
   identity/      transport/        eventlog/       discovery/    postoffice/
   ratchet/       (Noise XX)        (DAG + sync)    (signed UDP)  (elect+relay)
   channel/ dm/
        └────────────── storage/encryption.rs (PBKDF2-600k + AES-256-GCM at rest) ──┘
```

`mesh-talk-core` owns protocol state, network tasks, and encrypted persistence.
The desktop shell composes it using `RuntimeConfig` and `RuntimeEvents` in
`src-tauri/src/commands.rs`. `NodeRuntime::start` remains a compatibility
constructor for SDK callers. The shell owns the session and translates inbound
events to Tauri events; it does not own protocol or wire types. Desktop IPC
commands live in `src-tauri/src/chat_commands/`, grouped by account, messaging,
channels, files, platform, and diagnostics. `mod.rs` re-exports the established
command names for Tauri registration. Commands clone a node handle from
`NodeState` before asynchronous node work, releasing the session lock first.

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
  persisted so a restarted node can resume sending. An owner-authored `ChannelDissolve`
  event closes a channel for all members without erasing local history; closed channels
  remain visible as read-only history and reject new local events. Desktop-created
  groups mark the
  genesis and later invitations with `MTCH2`; invited devices remain pending until
  they append a self-signed `ChannelAccept`. The receiver surfaces content only
  when its signed event causally descends from acceptance, and an unaccepted member
  cannot send or inherit ownership. This cutoff also applies to channel file
  manifests, reactions, and recalls.
  Legacy unmarked channels retain their original auto-join semantics. The current
  owner alone may add/remove members or sign `ChannelOwnerTransfer`, and only an
  accepted current member can become owner. A non-owner can append a self-signed
  `ChannelLeave`; this advances the epoch and leaves its local history read-only.
  A concurrent owner snapshot cannot restore a departed member; re-invitation
  requires the snapshot to causally descend from the signed leave event.
  Invisible-mode admission and sync projection replay those same signed
  membership controls before granting a group route.
  Departure, acceptance, and ownership-transfer delivery are retried to channel
  members after an offline first push (bounded batches).
  Membership is keyed to the device identity, so another device of the same account
  must be invited separately. Restored groups remain read-only archives until that
  new device is invited. In Invisible mode, a manually trusted carrier may send
  a new channel's earlier signed ancestry, but the receiving sync connection
  stages it only in bounded memory (8 MiB). It is never advertised as durable
  custody and disappears on disconnect. A signed, causally valid invitation
  naming this device unlocks one atomic encrypted-log commit of the authorized
  ancestry and invitation. Group content still waits for explicit acceptance.
  A trusted post office may carry that ancestry without being a group member.
  After a signed removal, the removed device may retain the removal notice but
  cannot ingest later group content through a manually trusted member.
  If a channel has more than 8 MiB of ancestry before the invitation, this
  direct sync is rejected instead of persisting uninvited history; the inviter
  must create a fresh channel or a future protocol revision must carry a compact
  invitation proof ahead of the backlog.
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
  session guard. The host runtime state module owns the ticket, startup permit,
  installation check, and retirement sequence; IPC callers cannot access those
  synchronization fields directly. Replacement awaits the old runtime's consuming
  stop before opening any new profile. Guarded startup joins all already-started initializer
  writers on errors and authorizes the synchronous producer launch, installation,
  and each inbound callback against the current lease/ticket. A late rename cannot
  change a replacement session or runtime; startup uses the current matching name.
  Successful logout forgets the original login credential before awaiting teardown.
  On macOS, Stay signed in stores a password in the standard OS Keychain so a
  previously unlocked desktop can resume the session on launch. The in-app lock
  uses macOS device-owner authentication, which offers Touch ID or the current
  Mac account password without exposing that password to Mesh-Talk. An item saved
  by the earlier Touch ID-only release can migrate to the standard Keychain after
  one successful biometric read; if that read is unavailable, app sign-in still
  works. Sign-out removes both item types. Windows and
  Linux continue using their OS credential stores for remembered sessions.

  New `owner_*` delivery IPC captures the lease before waiting for the runtime
  lock. `OwnerAdmission` owns capture, checked reads, guarded local acceptance,
  and detached enqueue lifetime for these commands. Local text/sticker/file
  enqueue keeps that lifecycle admission through privacy updates and staging.
  Waiting for the privacy gate holds no session guard;
  once admitted, the matching session guard encloses the entire synchronous local
  grant (including verified bindings and routes), and separately the final WAL
  append. The grant and later staging/WAL are not one rollback transaction: a
  legitimately admitted grant can remain if the owner changes during staging.
  Admitted host enqueue tasks retain the lifecycle lock even if their IPC caller is
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
  Startup burst and scan times are measured from launch; a manual trigger works during
  startup. The fallback scan visits each /24 once and omits every local interface address.
- **Legacy/channel delivery** — these paths append the sealed event, then `deliver_direct` (Noise dial +
  one sync round) and, on failure/always, `replicate_to_post_office`. Receivers run an
  accept loop (`serve_connection` → `serve_one` ingest → `emit_new_messages` decrypt/surface).
  A background channel recovery tick also reconciles one joined channel with up to
  three known member routes. It rotates channels and peers, so a member that missed
  the original push can obtain signed events from another member even without a
  post office or the original author online. Each peer attempt is time-bounded;
  normal sync membership projection and event validation still apply.
  Discovery of a new or relocated peer wakes a bounded recovery batch immediately;
  the three-second tick remains a fallback for missed announcements and old routes.
  Every TCP-serving node runs this bidirectional anti-entropy loop. For channels
  with more than five members, a text sender seeds at most two routed members;
  recipients' rounds then forward the signed log to other members. Smaller
  channels and membership controls retain direct fanout. File manifests retain
  direct fanout until chunk replication has an explicit retention policy.
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
The delivery store derives outgoing and incoming File cards from the accepted
manifest identity. It prepares an outgoing card, local row, and fanout as one
transaction from the signed destination events and validated staged chunks.
That transaction journals the immutable fanout manifests, canonical local
file row and bounded destination/scope metadata before publication. Incoming
transactions install the file row before appending their immutable receipt.
Live recovery publishes callbacks once after durable installation; startup replay
only reconstructs history and pending controls. Enqueue APIs perform local durable
acceptance only; existing send APIs additionally attempt immediate transport.

Card delivery is not file download or read confirmation. Independent immutable
chunk work survives an early card receipt and sender restart. The worker also
rotates one file destination, sends its manifest before chunks, and retires chunk
work only after the exact target confirms the final event in the validated
dense signed chunk chain. The worker's file scheduler owns its pending and
active cursors, bounded transfer cache, liveness checks and retirement; journal
scans use an immutable cursor/capacity snapshot before network work resumes.
File chunk sync sends at most four ordinary Followup frames after each have-set
exchange. This reduces repeated id-set scans and round trips without changing
the wire format; an older responder already accepts each Followup independently.
The next exchange still confirms which signed chunks reached the recipient,
and exact final durable custody remains required before retiring source work.
A receiver may instead confirm historical verified
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
event sync frames before acceptance. MFM3 with per-chunk hashes remains the sender
format while it fits; larger files use compact MFM4, which retains indexed AEAD
per chunk and a whole-file SHA-256 checksum without the frame-sized hash list.
The receiver validates the declared chunk geometry and verifies the whole file
before exposing a save. Older clients continue receiving MFM3-sized files but
must upgrade to receive MFM4 offers. Empty files retain their one-empty-chunk encoding.

Directory attachments are packed into a tar payload and sent through this same
encrypted, chunked MFM3/MFM4 file path. A dedicated MIME marker identifies folders to
current clients; existing MFM3-capable clients still receive smaller folders as a normal
`.tar` attachment. The
sender bounds depth, entry count, path length and archive size and rejects links
and special files. The recipient first verifies the whole transfer, then extracts
only regular files and directories into a private staging directory, rejects
unsafe paths and oversized expansion, sanitizes cross-platform name components
with collision-safe mapping, and renames the completed folder into the chosen
download directory. Nested and empty directories are preserved. Chunk reclamation
waits until the extracted folder is durable so failed extraction remains retryable.

Incoming file, folder and video manifests are offers. The node stores a manual-by-default
receive policy in an encrypted, account-local sidecar; an explicit file decision overrides
the global policy and the sender-account override. Raster images sent as media are
automatic up to 16 MiB; SVG and larger images require approval. The recipient checks
the first decrypted chunk against the claimed raster signature before durably ingesting it,
while text and stickers remain ordinary message events. A current sender publishes the
manifest first, then asks the exact authenticated recipient device for consent before
synchronizing chunk events. The receiver independently rejects unsolicited chunks at its
durable sync gate, including chunk-first delivery from older senders. A channel sender
publishes the manifest and members pull chunks only after their own consent; automatic
images retain eager distribution. Decisions bind the signed manifest event and survive
restart. Identical signed manifests that share the same file conversation are aliases
of one immutable byte stream; one explicit decision covers those aliases while each
sender's automatic policy is evaluated separately. Exact decisions are retained
for the account lifetime, so later offers or policy changes cannot silently
reverse an earlier decline. Approved partial files are requeued on startup.
Offline recipients leave
the sender's chunks local until they reconnect and approve, so the sender must be
available later to finish delivery. The CLI exposes `/receive` and `/decline` for
manual decisions. Pre-consent peers retain the legacy direct-transfer fallback,
while the new receiver still applies its local gate.

File progress counts currently held chunks; historical completion does not
pretend that exported bytes remain on this node. The CLI keeps bounded pending
and recent-save queues and runs readiness checks and one export at a time on the
blocking pool. A retained managed-media copy can be exported after pruning;
every export verifies its size and checksum and synchronizes its output. Group
and own-device media use retained signed manifest/local-row references only for
local export, never for a historical network probe or a delivery receipt. Legacy
v1 exports keep their prior in-memory path and retain chunks rather than creating
new completion proofs.

The durable per-account `media/` store uses a versioned `MTMEDIA1` header and
64 KiB AES-256-GCM chunks. One password-derived key uses a private store salt;
each chunk authenticates the stable local device owner id, file conversation id, total length,
chunk index, and plaintext length. New media is written to a private `.part`
file, verified, synced, and atomically renamed. On account unlock, legacy
plaintext media is migrated through that path. A failed migration leaves the
source readable and surfaces a warning; old backups and snapshots remain out
of the app's control. Export decrypts through the store and still checks the
signed manifest checksum. Erasing the last local card referencing media removes
its managed copy, not an explicit user export.

The desktop host's privacy levels govern notification previews, automatic
locking, and copying. Rust persists the typed policy and sends one native
notification for each eligible inbound event. An `AppLock` gate blocks new app
IPC and plaintext events while keeping the node runtime alive; a native timer
checks idle and hidden policies even when the webview pauses. Unlock verifies
the active account with a password or, on macOS when enabled, the existing Touch
ID protected Keychain credential. Frontend ownership generations discard stale
UI work. Requests that began before a lock may still deliver a native IPC
response to the webview; do not treat the lock as a process-memory or OS capture
boundary.

Contact aliases, pins, trust records, local avatar overrides, and account settings
live in separate `accounts/<host user id>/{favorites,trust,avatars,settings}.json`
files. Their IPC commands
require the current host owner and keep session authority through each disk
operation, so a delayed command cannot follow a replacement login. The browser
keeps saved download locations under an owner-specific key and clears avatar
and settings mirrors on account changes. Installation-wide settings contain only
window behavior and saved-login bookkeeping; the saved username stays inside the
backend and is omitted from settings IPC responses. Older app-wide contact files are
moved to a private `legacy-unassigned` folder; their records contain no owner
that could support a safe automatic migration. Old unowned download locations
are similarly archived under a separate browser-storage key. Mixed legacy settings
are preserved in the private archive before account fields are removed from the
global settings file; those fields cannot be assigned to an account automatically.

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

Immutable file retries retain at most eight authenticated connections across
complete protocol exchanges, with a ten-second idle limit. A rotating admission
cursor fills available slots; each worker pass runs at most eight round-robin
file steps before continuing ordinary control work. Clean progress still retained
at pass end schedules another pass; failed attempts free slots without self-waking.
Dialing, fingerprinting, reconciliation and custody checking each have their
own clean exchange boundary under the same 400 ms operation budget. Each step
rechecks the current private authorization and exact destination binding; a
timeout discards the connection and any partial frame, resuming from durable
remote state on a fresh connection. Only final, target-qualified chunk custody
retires source file work. The SDK's immediate best-effort flush remains a
disposable, bounded attempt rather than retaining worker-owned state.

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
Manual permission publishes a signed listening route inside a pinned encrypted
connection, without requiring a message or a UDP announcement. The session-bound
private proof carries an authenticated visibility marker; receivers retain its
account proof and route but remove the peer from the online roster. Private
connections and periodic route probes never refresh visible presence. Older
clients that cannot verify the private proof reject the connection. Policy changes
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
probes refresh online presence only when a publicly visible pinned peer responds.
Private responses keep routing available without refreshing online presence.
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

**React 19 + TypeScript + Tailwind + shadcn/ui**, state in **zustand**, built with Vite.
`lib/api.ts` keeps the stable typed exports; `lib/api/` owns the feature-specific
`invoke()` wrappers and command payloads. `lib/events.ts` subscribes to
`dm-received`/`channel-message`/`file-received`.
`store/auth.ts` holds the session; `store/ownership.ts` validates owner and runtime epochs
for asynchronous frontend work. `store/outgoingIntent.ts` owns optimistic send admission,
backend acceptance, history loading, and delivery-status queries. `store/conversationState.ts`
owns cache eviction, deletion protection, exact-ID send completion, and history/status
state transitions. A late accepted ID after intent removal becomes a deletion tombstone;
completion never recreates an evicted conversation cache.
`store/chat.ts` exposes the UI-facing state and routes incoming DMs to the sender's
*account* (one conversation per multi-device contact). `features/chat/` is a two-pane app (conversation list ·
message view) with replies, reactions, @mentions, file send + a searchable received-files dialog,
search, and device linking; `features/auth/LoginScreen.tsx` is the only other screen.
`features/chat/useConversationViewport.ts` owns the virtual list's reading position,
restoration, search jumps and follow-latest callbacks across conversation switches.
The sidebar keeps Chats, Files, Connection, and Settings visible; its footer shows
the local identity and device/network status. The composer reveals secondary send
tools on demand and combines emoji and stickers in one picker. Settings uses
category navigation inside its dialog and reports persistence failures. The
empty conversation pane distinguishes no peers, hidden contacts, and a failed
contact-visibility read; Connection diagnostics distinguish loading from failure.
The chat roster and presence snapshot poll every 2 s. Unchanged roster snapshots
retain their existing references so a poll does not redraw conversation rows.

Unread counters are local presentation state. An incoming event in the selected
conversation remains unread while the window is unfocused or hidden; focusing that
conversation or opening another unread conversation clears only the one viewed
while the window is visible and focused.
`lib/unreadStorage.ts` restores opaque conversation keys and counts from the webview's
local storage, scoped by both the signed-in host user and the mesh account.
Messages and files from devices not yet in the local roster keep bounded pending
counts there until discovery maps each device to its account conversation. The
native badge includes these pending counts during discovery. This
metadata is stored locally without message text, is not encrypted, and does not
sync read status to other devices. A runtime account change discards the previous
account's in-memory selection and counters. The frontend's total drives both the
macOS menu-bar title and the OS app-icon badge through one session-guarded command.

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

### Manual encrypted backup and recovery

`recovery/{snapshot,archive,key}.rs` defines a versioned logical snapshot inside
an authenticated `MTBACK1` stream. Every export uses a fresh 256-bit recovery key;
the user saves that key separately from the `.mtbackup` file. The application never
stores the key. Archive chunks are bounded, encrypted with AES-256-GCM using an
HKDF-derived key, and closed by a separately keyed authenticated digest. A missing
final seal, wrong key, altered frame, duplicate record, or invalid signed event is
rejected before a restored profile can be published.

The desktop export command takes the same consuming runtime barrier as account
replacement: it stops producers and drains admitted work, opens the existing
profile offline, streams the snapshot to a restrictive sibling temporary file,
verifies it, then atomically publishes it without overwriting an existing file.
It restarts the runtime even when export fails or the IPC caller disappears.
The local last-export summary is password encrypted; it contains the path, time,
size, and counts, but never the key.

Restore is available while signed out. Inspection decrypts to private scratch,
checks the full archive and snapshot, and returns only a preview. Installation
creates fresh local auth credentials and a fresh device identity in an unpublished
directory, re-encrypts supported stores under the new password, then reopens both
auth and node stores for verification. A publication marker makes the two directory
renames recoverable: startup removes an incomplete pair or retains a completed pair
before auth initializes. Startup also removes orphan staging and decrypted scratch.
The recovered account fingerprint remains the same; the new device fingerprint
differs. Restored channels are read-only archives because their old sender keys
are excluded. Offline DM peers are discovered from durable account history.

Snapshots include visible plaintext history, signed event provenance, file cards,
managed media referenced by visible cards, received peer avatars, and recall marks.
Verified peer recall marks are persisted before export so a restored device does not
redisplay recalled plaintext it cannot decrypt from the old device's events. A
password-encrypted local index retains original IDs for restored outgoing messages;
the snapshot carries those IDs and the public author lineage needed to keep outgoing
file ownership correct through later backups and restores.
File-only account conversations carry their account association as a validated
snapshot record, retained in an encrypted local contact index after recovery.
They exclude local UI customizations (including the user's own avatar override),
old device keys, ratchets, channel sender chains, pending deliveries, routes,
global machine settings, and files managed outside Mesh-Talk. Each file card without local bytes remains
visible and is counted as missing media in the preview. Messages newer than the
backup require a later peer sync. Recovery cannot revoke an old certified device;
the UI sends users who suspect compromise to new identity creation.

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
- **Windows runtime**: `src-tauri/tauri.conf.json` selects static Visual C++ runtime
  linking with `build.windows.staticVCRuntime`. `src-tauri/build.rs` translates any
  legacy `STATIC_VCRUNTIME` value to the supported Windows build attribute before
  calling `tauri-build`, preserving the override without its deprecation warning.
- **Local gate** (`scripts/check-health.sh`): delivery / AI regression smoke,
  fmt,
  `clippy --workspace --all-targets -D warnings`, ESLint, frontend tests (Vitest), full
  `cargo test --workspace`, typos, cargo-deny, cargo-machete, gitleaks, shellcheck, audits,
  both builds — **mirrors CI** so failures surface locally, not on CI. The `hooks/pre-commit`
  hook runs a fast slice (`check-health.sh --fast`: fmt/clippy/lint/unit only); GPG-signed
  commits are enforced by `hooks/pre-push`.
- **CI** (`.github/workflows/`): `ci.yml` (Linux, macOS, and Windows matrix → the `verify` aggregate
  check, required by branch protection; coverage → Codecov on Linux), `check-health.yml`,
  `gitleaks.yml`, CodeQL, `dependabot-auto-merge.yml` (Dependabot PRs to `dev`, gated by required checks),
  `glib-0.20-watch.yml` (monthly dep watcher).
- **Automated bug-finding** (defence in depth — surfaces issues without anyone looking):
  - *Coverage-guided fuzzing* (`fuzz/`, `fuzz.yml`, weekly + dispatch) of selected public
    untrusted wire and opened-payload decoders; the `decoder_smoke` test is the always-on
    stable complement. See [`fuzz/README.md`](../fuzz/README.md) for the current targets.
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
- **Crypto primitives**: ed25519-dalek and x25519-dalek use `rand_core` 0.6
  directly; the desktop shell uses its own `rand` 0.10. Other primitives include
  aes-gcm, snow (Noise), sha2, hkdf, pbkdf2, and bincode.

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
