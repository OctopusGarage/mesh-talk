# Mesh-Talk Architecture

A decentralized, end-to-end-encrypted messenger. **Tauri** desktop shell, **Rust**
backend, **React + TypeScript** frontend. No vendor-operated server: LAN peers
discover each other over signed UDP **multicast** (group `224.0.0.167`, port
47474), connect directly over a Noise-encrypted TCP
channel, and store messages as an append-only, hash-linked **event log** that syncs CRDT-style.
When a peer is offline, a peer-operated **post office** node stores-and-forwards the (still-encrypted)
events. Existing routed networks can supply optional peer discovery; opted-in
post-office bridges introduce registered peers across sites without merging
their IP subnets. See [routed discovery](routed-network-discovery.md) and
[Network Topology and Communication Architecture](network-design-reference.md)
for cross-module design contracts, illustrated scenarios and development guardrails;
[network topologies](network-topologies.md) provides the compact overview.

> The earlier RSA-contact / plaintext-UDP / TCP-relay _legacy_ stack has been retired;
> this local-first stack is the entire product and lives at `/`. The only retained
> piece outside the node is the auth/session layer (`services/auth_service.rs` + `state.rs`),
> which `login` uses before starting the node.

---

Use the [canonical domain glossary](../CONTEXT.md) and
[DDD conventions](domain-design.md) for terminology and identifier translations.
This reference owns technical mechanisms and invariants.

## 1. Process & layers

See the [module ownership and reuse guide](module-boundaries.md) before assigning
parallel feature work or adding shared business logic. It maps current owners and
feature-local interfaces across the core, desktop host and frontend.

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
command names for Tauri registration. Sensitive IPC commands capture `RuntimeAdmission` before waiting on `NodeState`.
Blocking operations retain runtime lifecycle and matching session authority until
completion, even when their IPC waiter is cancelled. Asynchronous producers use
guarded core commit seams; plaintext queries recheck the originating admission
before returning results. A cloned node handle alone conveys no session authority.
The installed runtime slot uses shared operation leases and exclusive replacement
access. Network waits in one producer must not serialize identity, presence,
history or unrelated sends. Retirement still waits for every admitted lease;
session authorization continues to enclose synchronous commits.

## 2. Crypto & identity (`identity/`, `transport/`, `ratchet/`, `dm/`, `channel/`)

### Protected messages

Text and image messages can carry an authenticated `MessageProtection` policy: explicit
click-to-view, or authentication with the viewing device's existing app-lock
mechanism. Core history, search and received-event projections omit protected
content; explicit exact-message text and image queries are available to authorized
hosts. Ordinary image read and export APIs reject protected images.
The encrypted sent/received stores and eligible history replication retain the
full protected body. Ordinary text/reply and sticker wire layouts stay unchanged.

Protected wire bodies reserve an outer `MTEX1` zero-deadline sentinel, followed
by an optional ordinary nonzero-deadline frame and the strict versioned `MTPR1`
body. Existing expiry-aware clients reject the outer zero sentinel before their
raw-text fallback. The body carries base64 text so even clients predating expiry
framing do not render literal message text through that fallback. This encoding
is not encryption or access authorization; existing DM/channel encryption remains
the confidentiality boundary. Unknown or malformed protected bodies fail closed.
Updated clients preserve the nested absolute expiry independently of the
30-second local reveal. Older clients cannot reveal protected messages and may
display an empty or encoded item; there is no automatic ordinary-text downgrade.

Protected images wrap an existing encrypted file manifest in a strict versioned
`MFP1` protection envelope. Ordinary file manifests remain byte-compatible; older
clients cannot decode the protected wrapper and do not receive an ordinary image
downgrade. File chunks, integrity checks, transfer consent and encrypted media
persistence keep their existing owners. The canonical file binding retains the
protection requirement, so another filename or offer cannot grant ordinary access
to the same protected transfer. The explicit image reveal resolves the retained
message reference before reading image bytes. Ordinary media reads require a known
unprotected manifest; retiring metadata before blob cleanup cannot grant access.
Channel image admission retains the manifest and protection policy in the existing
delivery WAL. Recovery projects manifest history and retained chunk scope before
retiring the transaction; projection failure preserves the accepted message ID
instead of reporting a new send failure.

The desktop reveal command captures `RuntimeAdmission`, validates an exact
account/channel message reference, and reuses the app-lock credential verifier
when required. It checks the same desktop session, installed participant runtime,
lock state and retained message again after authentication. Recall, expiry,
logout or runtime replacement during the prompt prevents content publication.
The verifier does not unlock the app and its result is not shared across messages.

The frontend keeps the returned body in temporary message-view state for at most
30 seconds and clears it when the view, conversation or owner retires, or the app
loses focus or visibility. Late IPC results cannot restore a retired reveal.
Image previews use component-local Blob URLs, revoked when concealed or retired,
and never use the ordinary shared image URL cache.
Ordinary history IPC, accessibility summaries, notifications, conversation
previews and reply quotes carry placeholders. This is a supported-client
presentation control, separate from encryption at rest, message expiry and
retention; it cannot prevent a recipient from modifying their client or capturing
already displayed content. See [protected-message behavior](privacy-controls.md#protected-messages).

### Fixed-deadline messages

The composer optionally selects a per-message duration and computes
one UTC expiry timestamp at send time. The timestamp is inside authenticated DM,
channel, or file-manifest plaintext (`MTEX1` frame), so every recipient device
receives the same deadline. Both sender and recipient history and search exclude
expired content; incoming notifications exclude expired arrivals. History IPC
returns the authenticated deadline for per-message countdowns. A single desktop
session monotonic clock removes expired cached messages and outgoing intents and updates
countdowns without per-message IPC. Focus and visibility changes recheck deadlines;
the history dialog filters and clears its own expired rows.
A one-second core maintenance task removes plaintext sidecars on both sides,
cancels expired outgoing retry work, suppresses replay through account sync, and
removes locally managed media. Queries enforce deadlines independently of cleanup;
cleanup errors are logged and retried on the next maintenance pass. A monotonic
observation timer also bounds the lifetime by the sender event's intended duration if the local
wall clock was already wrong when the message arrived. On restart the prior monotonic
clock anchor cannot be recovered; expiring recipient copies are erased early to
avoid extending their lifetime after a backward clock change. This is a local
retention feature: a recipient who controls their device can copy content or
modify the client, so remote deletion cannot be guaranteed. Older clients do
not understand the `MTEX1` frame and may keep its decrypted bytes; only updated
devices enforce expiry. Delayed delivery never restarts the countdown, and
already-expired arrivals are hidden. Sender copies keep their deadline across
restart; conservative early deletion on restart applies to recipient copies only.

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
  fresh DH chains provide forward secrecy + post-compromise recovery, subject to
  the bootstrap limitation below; bounded out-of-order (1000/2000); lower
  `user_id` is the canonical initiator (simultaneous-init tie-break); state encrypted on
  disk (`node/ratchet_sessions.rs`). The tie-break may replace an initial sending
  session only before an authenticated receiving chain exists; replayed ciphertext
  must not reset an established session, including after reload.
  Bootstrap uses the recipient's permanent X25519 key, so later compromise of
  that key can recover recorded messages in the initiator's first sending chain,
  until the initiator processes the responder's fresh DH header. Later chains
  require the corresponding ephemeral secrets; this is not whole-session exposure.
  The `dm.rs` recipient sealed-box (no FS) is now used only
  to distribute channel keys, seal file manifests and encrypt delivery-control
  metadata (the latter has no forward secrecy and contains no message text).
- **Shared group metadata** — owner-published group photos and announcements use the appended kind-19
  metadata control, gated by V5 sync envelopes. Each publication encrypts its
  bounded content under an independent random key sealed to accepted members;
  eligible current holders can release only a current field's key to an accepted
  newcomer. This does not export prior sender chains or enable pre-join text
  history. Causal owner authority, exact device/account bindings and current
  membership govern publication, replay and release. Independent field revisions
  and durable clear tombstones preserve offline convergence without coupling
  updates to membership epochs or ordinary message retention. Private avatar
  overrides retain their account-document owner. See [group announcements](help/topics/channels.md#group-announcements).
- **Channels** — per-sender **sender-key** group ratchet (`channel/sender_key.rs`):
  single-use message keys; membership add/remove rotates the **epoch** and re-distributes
  sender-key distributions (sealed per member via the DM sealed-box). Sender chains are
  persisted so a restarted node can resume sending. The static-recipient sealed
  distributions remain recoverable after member identity-key compromise, so
  deleting used chain keys does not protect recorded group history against that
  attack. Receiver state currently rebuilds by reopening distributions from the
  event log; replacing this with consuming pairwise ratchets requires a durable,
  transactional receiver projection that preserves restart and replay behavior.
  Opening text or a file manifest first binds the authenticated author and epoch,
  then prepares an exact private receiver candidate. Only successful AEAD installs
  it; failed authentication and abandoned preparation preserve the live chain.
  The bounded device-local codec preserves the next position, skipped keys and
  exact eviction order, rejects malformed or oversized input, and wipes temporary
  secret buffers. It is neither a wire distribution nor a portable backup format.
  Sender and receiver advances reject exhausted `u32` positions before mutation;
  already-buffered skipped keys remain usable at the terminal position. These
  primitives do not yet persist receiver checkpoints or atomically commit crypto
  with transcript installation, and do not remove startup history replay.
  An owner-authored `ChannelDissolve`
  event closes a channel for all members without erasing local history; closed channels
  remain visible as read-only history and reject new local events. Desktop-created
  groups mark the
  genesis and later invitations with `MTCH2`; invited devices remain pending until
  they append a self-signed `ChannelAccept`. The receiver surfaces content only
  when its signed event causally descends from acceptance, and an unaccepted member
  cannot send or inherit ownership. This cutoff also applies to channel file
  manifests, reactions, and recalls.
  A separately owner-authorized [join-history copy](help/topics/channels.md#history-for-new-members)
  is the bounded exception to the acceptance cutoff. Append-only event kinds 15/16
  carry a signed policy and versioned offer/release controls; V3 channel sync gates
  them from older decoders. Invitation and offer use the existing durable delivery
  journal as one transaction. The offer freezes a capped selection of retained text
  at the invitation, with original event IDs and expiry framing. A fresh snapshot
  root derives independent per-message encryption keys via domain-separated HKDF;
  only existing holders authorized for every row receive the root, sealed per
  device. After signed acceptance, an eligible sharing holder seals only unexpired,
  nonrecalled row keys to the new device. The release observes both offer and
  acceptance, which may arrive in either order. Removal ends that invitation's
  permission lifetime; stale offers do not generate retry events. Recipients
  validate canonical owner authority, original event ancestry, count/time limits, lifetime,
  release authority and encryption before using the existing durable received
  projection. Ownership transfers pin observed policy/offer ancestry; former-owner
  forks cannot override the new owner or backdate unobserved grants into a closed
  owner lifetime. These are owner-attested copies of text, not independent proof that
  the original author encrypted that exact plaintext. Sender chains, attachment
  admission, ordinary post-acceptance content and existing plaintext archive rules
  remain owned by their existing modules.
  Account-authorized channels append root-signed account grants in kind 17 and
  full-device account attestations in V4-gated kind 18. Membership, owner roles,
  consent and revocation apply to an account grant lifetime. Certified devices
  inherit accepted roles without individual owner approval and keep independent
  sending chains. Initial migration is a new owner-authorized commitment, using
  local creation/kept-source provenance and authenticated full-key account proofs;
  later device account adoption cannot relabel an established grant.
  For an account invitation, invitation, pending grant and selected history offer
  commit atomically through the same delivery journal. Any certified device of
  that account can accept. A bounded `MTJHA1` release in kind 18 binds the frozen
  offer, exact grant incarnation, causal acceptance and target device attestation,
  so an eligible sharing holder can send only selected row keys even when the
  invitation's original device is offline. Legacy offer/release bytes remain
  unchanged; account releases retain the original expiry, recall, protection,
  count and ownership lifetime checks. See the
  [account membership contract](network-design-reference.md#d-group-creation-distribution-and-member-recovery).
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
  Membership and ownership in account-authorized groups belong to accounts.
  New desktop groups authenticate selected account IDs and commit account grants
  at creation. An invitation is accepted once per account; certified linked
  devices inherit the role through authenticated sibling synchronization without
  another owner invitation. Legacy device-only groups retain their signed device
  lifetimes until an authorized account-format migration is available. A restored
  archive alone does not prove membership. See the
  [account/device boundaries](multi-device-sync-design.md#account-and-device-decision-boundaries).
  In Invisible mode, a manually trusted carrier may send
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
  sender/recipient _account_ routing inside the ciphertext; `send_to_account` fans a
  per-device ratcheted copy to eligible devices of the target account and to own
  devices when linked-message history is enabled; `account_history` merges by `msg_id`. Device **linking** (`node/pairing.rs`):
  one-time 128-bit code (SHA-256 authenticator binding both device keys, constant-time
  check) → account secret + cert + history backfill transferred over the Noise channel.
  Desktop enrollment uses `MTPQ4` / `MTPS4` and requires the source's signed
  private account-access snapshot before adopting account keys. Desktop linking
  never downgrades to V1/V2/V3; immutable creation metadata remains included,
  and historical SDK/CLI V1/V2/V3 paths remain compatible.
  `node/account_access.rs` owns bounded opaque account-access records, encrypted
  persistence, account signature/revision validation and authenticated certified-
  sibling exchange, independently of optional synchronization categories. Only
  the explicit keep-account action seeds source credentials; joining startup
  never publishes its previous login. Public profiles, discovery, contact remarks
  and ordinary chat events never carry passwords.
  `identity/account_login.rs` owns canonical sign-in resolution through an atomic
  versioned per-local-user envelope. Its encrypted payload repeats the public
  account/local-user binding and retains separate identity-auth and raw node-
  storage passwords. Shared sign-in changes neither the local-user UUID nor the
  existing storage username and never re-encrypts retained history. Migrated users
  cannot authenticate through the old login; corrupt envelopes fail closed.
  Registration and migration share a namespace lock and reject canonical-login
  collisions without merging users or overwriting existing accounts.
  `src-tauri/src/shared_account_login.rs` owns private desktop payloads, source
  initialization, pre-adoption collision checks and admitted snapshot application.
  It checks the current session after encryption and publishes the envelope under
  that authority. Cross-account replacement additionally requires the matching
  persisted previous-account/next-account/device adoption intent. Session and
  saved-login projections use the canonical account login, while retained storage
  credentials remain device-local. The code issuer keeps its account and login;
  the joining desktop explicitly consents to use that login name and password.
  Self-account sends also work with no known siblings: the delivery WAL accepts a
  durable local projection without reserving a remote queue slot. Message Yourself
  reuses the self-account conversation and logical message IDs. Personal file
  cards retain encrypted managed content and a detached signed manifest proof
  (`MTPFM1`) binding the exact expiry-wrapped manifest digest to that conversation.
  The proof is evidence, not an event grafted into the event-log DAG.
  Account-history protocol `MTAHS2` separates file cards from file bytes and
  supports personal file proofs; `MTAHS1` retains its frozen legacy encoding and
  attachment-coupled cards. Pairing request `MTPQ2` authenticates the requested
  history choice before transfer; strict no-history enrollment never falls back
  to the legacy request. Source and receiver policy revisions guard each page
  and final attachment commit. History-off pauses sibling message decryption;
  retained encrypted events can be processed after re-enabling. Ordinary external
  messages and group membership authorization remain independent. Signed sibling
  profile updates are independent DAG leaves, so withholding message/file
  ancestors does not stall an enabled avatar category. Reconciliation compares
  event ID sets and supports sparse author sequences.
  Durable imported projections advance a process-local history revision exposed
  by the owner-scoped identity command. The existing UI poll refreshes the open
  conversation on change; own-device copies do not generate incoming DM alerts.
  Durable enrollment permits only the exact certified Ed25519/X25519 device to
  adopt the owning account; fresh self-signed presence alone cannot change a
  pinned account. Startup reconstructs the expected binding and retires old
  roster presence. Enrollment invalidates pre-transition managed sessions and
  consumes the code atomically; only its current pairing response channel gets
  a renewed guard. Interrupted adoption can retry with a new valid code through
  a restricted incoming pairing-only channel; that exception never serves
  ordinary sync, calls, attachments or account history before code validation.
  Message history, attachment catch-up, signed account-avatar updates, appearance,
  and contact presentation preferences obey separate device-local switches.
  Appearance and contact records use a versioned, encrypted local store with a
  deterministic revision/device tie-break and clear tombstones. Only certified
  sibling devices exchange these records over the Noise account-sync channel.
  Successful device-local switch changes invalidate account-history cursors and
  cached digests through a process-local policy revision. A page interrupted by
  a switch change, including an off/on toggle, cannot acknowledge skipped data
  as synchronized; the next pull starts a fresh sweep. Policy revisions do not
  change the persisted policy or wire format.
  Profile updates use the existing per-peer
  event delivery path; a device with avatar sync disabled defers sibling updates
  until it is re-enabled. The received-profile store retains clear tombstones,
  and each device also stores its own signed profile state so it can republish
  the current avatar or clear after restart. Channel owners invite certified
  siblings of accepted members; a sibling accepts automatically only when an
  already accepted member has its own certified account. Once accepted, bounded
  account reconciliation transfers available older channel plaintext with stable
  event IDs. Locally erased channel messages carry durable suppression markers.
  Historical file cards carry their signed manifest event as a detached proof when
  the original device-pair event DAG is unavailable. A sibling can request an
  available attachment through a separate authenticated stream; the receiver checks
  the manifest size and SHA-256 before encrypting its local copy. Complete local
  chunk transfers are likewise verified and copied into encrypted managed storage
  while attachment sync is enabled, so a later generic-file save cannot remove
  the linked device's only retained bytes. File cards and
  bytes remain local to eligible account conversations and accepted channels.
  The file-card sidecar permits identical signed manifest content under both a
  device conversation and its account conversation; the message sidecar still
  rejects reused event IDs across conversations.

## 3. Event log & sync (`eventlog/`)

Group presentation traverses the retained, validated parent graph through
`node/timeline.rs`; author timestamps remain display metadata and deterministic
tie-breaks among concurrent ready events. This does not rely on a peer's signed
Lamport value being monotonic. Plaintext-only account archive rows have a
timestamp/ID fallback until signed ancestry is available. The desktop reception
contract carries event identity, local observation and processing-batch context
for bounded unread reconciliation, calm notifications and message-anchored
history insertion. See [group conversation reception](group-conversation-reception.md)
for user behavior, compatibility and resource limits. A successful bounded peer
exchange is an observation, never a claim that offline members have no more data.

- **Event** — content-addressed: `id = SHA-256(domain ‖ content)`; fields `conversation,
author (Ed25519 pub, self-certifying), seq, parents (→ hash-linked DAG), lamport,
wall_clock, kind, ciphertext, sig`. `EventKind`: Message/Edit/Delete/React/ReadMarker/
  MembershipChange/KeyRotation/FileManifest (indices frozen for wire stability).
- **EventLog** (in-memory, `store.rs`) — per-conversation DAG; tracks `heads` (frontier),
  `version` (per-author max seq → equivocation/fork detection), `max_lamport`. `events()`
  returns `(lamport, id)`-sorted (deterministic logical-clock order). Because peer
  Lamport monotonicity is not validated, group consumers use the validated-parent
  traversal in `node/timeline.rs` when they require causal order.
  Retained-log compaction rebuilds causal indexes through `eventlog/order.rs`
  before atomically replacing durable records; the timeline facade reuses this
  traversal. Public Rust API migration: `EventLog::prepare` and
  `PersistentEventLog::prepare` now return `Result<(Vec<EventId>, u64), LogError>`.
  Callers must propagate or handle `ClockExhausted`; `next_sequence` returns
  `SequenceExhausted` when the author counter has no successor. Exhaustive
  `LogError` matches must account for these variants. Event serialization and
  wire formats are unchanged.
  Individual durable lookups `PersistentEventLog::get` and `authored_event`
  return `Result<Option<Event>, LogError>` with an owned, complete event. The
  in-memory `EventLog` keeps its borrowed API. Durable collection, membership,
  conversation metadata and capability reads also return `Result`; `events`
  returns owned events. Only `Ok(None)` permits an
  absence-based fallback; a failed read must stop authorization, media reclamation
  or the affected delivery/recovery attempt without advancing its progress.
- **Sync** (`sync.rs`) — three-message id-set reconciliation: `Request(have)` →
  `Response(missing + responder have)` → `Followup(what responder lacks)`, bounded by
  frame budget (`MAX_PLAINTEXT`), multiple rounds (≤ `MAX_SYNC_ROUNDS`). Every event is
  re-verified (hash + signature) on ingest.
  Storage reads and reconciliation handlers return `Result`; a local read,
  synchronization or persistence failure terminates the exchange through
  `SessionError::Store`, preserving its error source. It must not become an empty
  event set, a matching fingerprint or a positive custody claim. Invalid incoming
  events remain bounded per-event rejections; explicit policy denials use
  `LogError::AdmissionRejected`. I/O error kinds do not identify policy denials.
  A failed batch can retain its already committed prefix; retry uses the existing
  content-addressed duplicate check rather than assuming the batch rolled back.
  This changes the Rust storage interface, not event IDs or the wire encoding.
  Channel recovery asks up to three online members per turn and continues past a
  reachable but stale member. A completed generic channel attachment remains available from a
  receiver after local save; complete attachment chunk conversations are retained
  newest-first within a 64 MiB ciphertext budget, then whole old files are evicted.
  Each accepted member shares its channel log and retained channel file chunks by
  default. A member can publish a self-signed `ChannelReplica` preference (event
  kind 14, one-byte enabled flag). Peers show the signed status in the member list.
  Disabling sharing hides the local channel and channel-file logs from inbound
  reconciliation while preserving incoming pushes and the member's local history.
  Recovery becomes pull-only; outbound control retries exclude retained third-party
  events. Own events can be sent only when their parents are already held by the
  recipient or are eligible own events in the batch. Otherwise the own event waits
  rather than disclosing third-party ancestry. Public presence does not authorize
  private channel access: signed membership is replayed even before ChannelBook
  refresh, and an established genesis-owner pin cannot be replaced by a competing
  signed root.
  The latest local preference is retried as a control event, so a member that was
  offline when it changed can learn the status after reconnecting.
  Because event-kind indices are positional in bincode, sync preflights the
  channel version before transferring kind 14. A requester that holds a replica
  preference starts with appended v2 sync frames; an older peer rejects those
  frames and the exchange closes before the preference is transferred. A newer
  responder with kind 14 answers a legacy fingerprint/request with an appended
  `UpgradeRequired` frame; a newer requester retries using v2, while an older
  requester closes. Channels without kind 14 keep their legacy sync frames.
  A mixed-version group can therefore stop syncing that channel after a
  replica preference is published. Upgrade all members before using the
  replica toggle; this is a safe compatibility failure, not seamless interop.
  Channel message sends ask each seeded member for an authenticated, exact
  `DurableHave` response after synchronization. The sender records distinct
  confirmations in a local password-encrypted `channel.custody` log. Its own
  signed message events are the durable retry queue: after restart and on
  discovery, a bounded recovery tick retries messages with fewer than two
  confirmed, currently sharing members (or fewer if the group is smaller).
  An ordered message-by-author index lets each tick inspect at most 128 of the
  sender's events per selected channel without rebuilding a whole-history list.
  Cursors rotate channels and resume within their histories. Each examined
  message advances the retry cursor so a failed sync cannot starve later
  messages; custody retries rotate eligible members, while rechecks rotate
  confirmed members for each message.
  The UI shows no-confirmed-copy, last-confirmed one-copy, or last-confirmed
  two-or-more-copy status for the sender's message. Every 20 recovery ticks,
  one historical claim is probed without first sending the event; a negative
  durable answer is recorded as a revocation and normal retry can restore it.
  An offline peer cannot be rechecked, so the count is still historical evidence,
  not proof that a peer currently has the event, nor a delivery or read receipt.
  Post offices are not counted as group-member copies.
  Certified devices of the sender's account share custody observations through
  the existing private account-document session. Observations name the exact
  channel, event and holder; account-authenticated revisions and negative
  observations prevent stale positive replay. They attest to an account device's
  authenticated durable-have check, not an independently signed holder receipt.
  Status queries also recognize retained outgoing history authored by a certified
  sibling. Counts group accepted external member accounts once and exclude all
  sender-account devices; legacy membership retains verified device fallback.
  Missing observation data is unknown, rather than evidence of zero copies.
  The optional custody domain requires an upgraded sibling while existing
  supported document domains continue with older peers.
  The ignored `channel_replica_scale_rehearsal` test runs a repeatable 5/20/50
  member Noise duplex exercise with every fifth recipient initially offline,
  then recovers them from another member after the origin exits. It reports
  contact count, serialized event payload estimate, and convergence time.
  `channel_real_tcp_scale_rehearsal` runs complete Nodes over loopback TCP and
  measures encrypted application-frame bytes sent by the origin to its initial
  targets. See [channel release rehearsal](channel-release-rehearsal.md) for
  the repeatable commands and mixed-version binary check.
  This protocol rehearsal does not measure real LAN discovery, TCP overhead,
  or sustained throughput; use the sync-cost debug metrics and a real LAN trial
  before deciding whether a fuller gossip protocol is needed.
  Locally saved channel attachments remain available as bounded complete-file
  replicas until the channel-file budget evicts older files.
  Direct-message files and inline media keep their existing completion and
  cleanup paths.
  The durable log caches its raw conversation digest. A separate, bounded
  128-entry principal-scoped cache stores only the public authorized projection's
  fingerprint, count and sync-version requirement. Its key includes that raw
  digest/count, exact identities/account, established owner/membership, direction,
  sharing policy and privacy generation/configuration. Append, compaction, book
  refresh, removal, sharing and privacy changes invalidate reuse; a fresh
  connection to an unchanged channel avoids cloning ciphertext or replaying history.
  Invisible-mode projections remain uncached and include only authorized events.
- **Persistence** (`persist.rs`) — `MTLOG1 ‖ salt ‖ [len ‖ nonce ‖ AES-256-GCM(event)]…`,
  append-only; a torn trailing record (crash mid-write) is dropped on reload and re-synced.
  Profile sync queues compaction requests. `Node::run_accept_loop` owns a maintenance
  loop shared by desktop, CLI, and SDK hosts: every 3 seconds it coalesces eligible
  conversations into at most one cost-bounded rewrite on the blocking pool. The
  [storage budgets](#storage-replay-and-query-budgets) govern eligibility and cadence.
  Dropping the accept-loop future stops scheduling maintenance; an already-running
  blocking rewrite finishes atomically. The log mutex serializes rewrites with appends
  within one Node, but is not shared with a replacement Node opening the same files.
  CLI and desktop share `PreparedParticipant`/`ParticipantRuntime` for discovery,
  accept, private-route probes, mailbox drain, bridge refresh and pending-file pulls.
  Preparation binds resources without launching producers; desktop launch remains
  inside its final session authorization. Profile paths, event callbacks and REPL
  behavior stay with their host. Extra host tasks are adopted by the shared owner;
  CLI autosave blocking closures carry retirement permits.
  Hosts must await consuming `NodeRuntime::stop` (or `ParticipantRuntime::stop`
  for direct core hosts) before reopening a runtime profile:
  it closes producer admission, aborts and joins the runtime tasks, cancels owned
  accepted connections and private-route probe children and waits for their actual
  termination, and waits for admitted
  blocking profile rewrites, delivery recovery, peer-cache writes and file staging
  to finish.
  Runtime Drop only requests cancellation and cannot provide that retirement barrier.
  Participant constructors also retain the cooperating
  [profile lease](#storage-replay-and-query-budgets). External SDK operations and
  host file staging still require the host's own lifecycle serialization; an
  independently retained Node keeps its profile busy after runtime retirement.

  `RuntimeStartup` owns construction admission before keystore work begins.
  `StartupCompletion` waits for the construction future and every admitted
  blocking initializer, including canceled waiters. Blocking captures and
  undelivered results release their resources before completion admission ends.
  The desktop retains replacement serialization through this completion.
  A successfully returned runtime retains the ordinary consuming stop contract;
  construction completion does not retire that runtime or grant session authority.

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
  An IDs-only startup observation may publish the device/account identity after
  the existing keystore unlocks, before durable stores finish opening. The host
  binds it to that exact lease and startup ticket; replacement and failed or
  cancelled startup revoke it. This observation permits account-scoped appearance
  cache hydration only. It does not install a runtime or admit messaging/settings
  commands. Full node identity and readiness still require runtime admission.
  Successful logout forgets the original login credential before awaiting teardown.
  On macOS, Stay signed in stores a password in the standard OS Keychain so a
  previously unlocked desktop can resume the session on launch. The in-app lock
  uses macOS device-owner authentication, which offers Touch ID or the current
  Mac account password without exposing that password to Mesh-Talk. An item saved
  by the earlier Touch ID-only release can migrate to the standard Keychain after
  one successful biometric read; if that read is unavailable, app sign-in still
  works. Sign-out removes both item types. Windows and
  Linux continue using their OS credential stores for remembered sessions.

  `owner_*` delivery IPC and legacy account text/sticker/file sends capture the
  lease before waiting for the runtime lock. `RuntimeAdmission` owns capture,
  checked reads, guarded local acceptance and detached producer lifetime.
  When contact presentation needs both runtime and policy state, acquire the
  runtime lock, then the contact-policy lock, then session authority. Never wait
  for a runtime while holding the session lock: startup installation and admitted
  producers also need that session. Presence and self-contact identity are still
  read inside the authorized policy transaction, with the runtime retained.
  Local send preparation and journal acceptance always enter host authority
  before the delivery journal; no producer retains that journal while requesting
  session authority. This ordering also covers cached retry lookups and prevents
  concurrent signed-card/legacy-avatar publication from forming a lock cycle.
  Local text/sticker/file
  enqueue keeps that lifecycle admission through privacy updates and staging.
  Waiting for the privacy gate holds no session guard;
  once admitted, the matching session guard encloses the entire synchronous local
  grant (including verified bindings and routes), and separately the final WAL
  append. The grant and later staging/WAL are not one rollback transaction: a
  legitimately admitted grant can remain if the owner changes during staging.
  Admitted host enqueue tasks retain the lifecycle lock even if their IPC caller is
  cancelled. Legacy account sends retain immediate best-effort transfer and their
  original results. Their payloads lack an explicit frontend owner, so capture
  authorizes the host session at IPC entry; it cannot identify an intent created
  in an earlier frontend session. Privacy commands use this same admission for
  observations and synchronous commits after the async policy gate. Their guarded
  core methods enclose policy, proof and route writes plus generation updates and
  bridge invalidation; they enter host authority outside privacy-state locks.
  Announcement drain/restore and presence publication remain awaits outside the
  session guard. A canceled IPC waiter cannot release the lifecycle barrier while
  its detached privacy operation still runs. Registered channel/device/file,
  reaction and recall mutations use guarded synchronous commits as well. Call
  signals authorize Noise frame encryption after policy waits; denial consumes
  no nonce. Contact mutations retain the entry lease through runtime/policy/session
  admission. Live settings application is serialized and revision-checked at
  policy and retention commits, including the periodic prune. Arbitrary SDK
  handles are not session leases. Stable file results include the original
  card ID and file conversation.
  Owner-sensitive identity queries atomically inspect session plus runtime; status
  queries accept at most 256 IDs and project only the exact account conversation.
  Own-device synchronization, device-addressed legacy history and incoming rows
  have no external delivery status. Existing SDK and legacy IPC response shapes
  remain available; new frontend flows must use the owner-sensitive surface.

Own DM/account reactions are retained in an account-scoped encrypted latest-action
projection. Restart and offline discovery do not discard the projection; local
message erasure and retention remove its matching rows. Repeated toggles compact
into their latest actions. Reaction fanout remains best-effort and is not part of
the message delivery WAL; portable recovery snapshots currently omit reactions.

## 4. Networking & delivery (`discovery/`, `node/`, `postoffice/`)

- **Discovery** — signed `Announce` (Ed25519, version 2) carries `x25519_pub`, `tcp_port`,
  `post_office` flag, and the `account_cert`; the device signature commits to the account
  key (prevents cert-swap). Transport is UDP multicast (`DISCOVERY_MULTICAST_GROUP`
  224.0.0.167, `DEFAULT_DISCOVERY_PORT` 47474 in `transport/net.rs`) joined on every IPv4
  interface, plus a unicast announce/response reply, a /24 unicast scan fallback, a startup
  burst, and periodic re-join. `run_broadcast` re-announces every 2 s; `run_listen` verifies
  - updates the roster; TTL eviction; `devices_of_account()` groups devices by account.
    Startup burst and scan times are measured from launch; a manual trigger works during
    startup. The fallback scan visits each LAN /24 once, omits every local interface address,
    and excludes Tailscale's 100.64.0.0/10 overlay range.
    Desktop builds can additionally obtain candidate IPs from an already-running local
    Tailscale or EasyTier client through host-owned adapters. Both inventories run
    concurrently with independent deadlines and share a deduplicated, interleaved
    256-address budget; a user can also enter an address. A rate-limited, identity-free UDP
    query elicits a fresh nonce-bound signed announce from visible Mesh Talk nodes;
    these endpoints remain separate from device identity and from Noise authorization.
    A UDP peer reset or refusal does not stop the listener: Windows can report a
    departed peer's ICMP port-unreachable response as receive error 10054. Local
    socket failures remain fatal and are logged with their OS error.
    The roster and encrypted route cache retain multiple addresses per device and
    a pinned dial staggers up to two candidate handshakes by 200 ms, with a
    ten-second authentication deadline per candidate. Every attempt owns separate
    TCP capacity permits; cancellation drops losing attempts. An obsolete LAN
    endpoint accepting TCP through a proxy cannot block the first alternate route
    for a full handshake deadline. Reusing an existing durable contact grant does
    not acquire the disclosure gate's writer or repeat policy/proof persistence;
    new grants and binding changes retain the serialized commit path and host
    authorization. Slow local text acceptance, route authentication and delivery
    attempts emit elapsed-time diagnostics with stage/outcome and route endpoint
    metadata, excluding message bodies and identity identifiers. Shared discovery observes interface names,
    addresses and indexes every two seconds; changes rejoin multicast and wake
    privacy-gated LAN/routed producers without restarting the participant. A
    30-second multicast retry remains as a fallback. See [routed network discovery](routed-network-discovery.md)
    for the protocol, limitations, and cross-network release checks; see
    [network topologies](network-topologies.md) for the A–F deployment scenarios.
- **Automatic account delivery** — after a direct durable-have response, the sender
  also requests the intended device's authenticated receipt scope over the same
  reachable path, within the existing control-attempt deadline. A recipient need
  not establish a reverse connection before confirmation can return. Custody
  alone never becomes a recipient-confirmation claim. Live account DM notifications
  carry the authenticated sender account and logical envelope message ID through
  desktop IPC; their unread projection shares that ID with account history.
- **Legacy/channel delivery** — these paths append the sealed event, then `deliver_direct` (Noise dial +
  one sync round) and, on failure/always, `replicate_to_post_office`. Receivers run an
  accept loop (`serve_connection` → `serve_one` ingest → `emit_new_messages` decrypt/surface).
  A background channel recovery tick also reconciles one joined channel with up to
  three known member routes. It rotates channels and peers, so a member that missed
  the original push can obtain signed events from another member even without a
  post office or the original author online. Each peer attempt is time-bounded;
  normal sync membership projection and event validation still apply.
  Discovery of a new or relocated peer wakes a bounded recovery batch immediately;
  an adaptive jittered three-to-sixty-second recovery fallback repairs missed
  announcements and old routes; custody checks retain their independent tick.
  Every TCP-serving node runs this bidirectional anti-entropy loop. For channels
  with more than five members, a text sender seeds at most two routed members;
  recipients' rounds then forward the signed log to other members. Smaller
  channels and membership controls retain direct fanout. File manifests retain
  direct fanout until chunk replication has an explicit retention policy.
  Initial text/sticker contacts run concurrently (at most four for a small channel,
  two for a larger one), so a stalled handshake cannot delay another member.
  Membership and key controls contact every routed member with a four-connection
  concurrency cap and a per-peer deadline. Newly validated messages wake a
  dedicated coalesced recovery signal; duplicate processing and empty rounds do
  not wake it. Identity/channel rotation distributes the bounded three-peer
  recovery rounds. Sequential-sync operations use a Node-owned authenticated
  session pool. A shared
  eight-slot TCP budget covers both directions, handshakes and idle sessions;
  seven-slot direction limits preserve opposite-direction progress. Idle sessions
  expire after 15 seconds. Full identity, endpoint, account/role and policy checks
  prevent cross-principal or stale-policy reuse. Negotiated V2 sibling history,
  attachment and wake requests share a separate reusable protocol class;
  older peers retain fresh exchanges. Call, pairing and directory protocols
  dial fresh; errors and cancellation discard sessions. One idle entry per identity/route/protocol
  does not imply one busy connection per peer. OS backlog, discovery UDP, call
  media and separate relay processes are outside the Node budget.
  Each channel send emits a debug metric with member count, seeded peer count,
  confirmed member copies, and elapsed milliseconds. The sync layer separately
  logs have-set size, applied and pushed events, and round count. These provide
  latency and metadata-bandwidth evidence before changing the fanout threshold
  or introducing a more complex gossip protocol.
  Member copy counts are historical confirmations. To make a current durability
  guarantee, the protocol would need renewable leases or periodic exact probes
  and a policy for peers that permanently leave; the UI does not claim one.
- **Post office** — verified relays are ordered by an optional user preference,
  then direct LAN reachability, then fingerprint. Introduced recipients favor
  the relays that reported them; `drain_from_post_office` visits every known
  mailbox every 3 s. Each account may disable
  relay use in Settings; the default remains enabled. Independent sending and
  receiving preferences default to true under that master switch. Core policy
  gates all opaque messages, file consent, receipts and chunk recovery. Receive
  opt-out rejects content admission and durable seen state, while the unchanged
  bidirectional protocol may still return encrypted frames. Disabling sending
  preserves local queued work and prevents outgoing relay controls as well. New clients wrap signed events for each recipient
  in a pairwise opaque mailbox before relay sync. The relay sees mailbox IDs,
  pseudonymous outer headers, size buckets, and traffic timing, but cannot
  open event headers, channel controls, DM text, or file contents.
  Opaque wrapper IDs do not preserve inner causal order. A received event whose
  parents are missing is deferred for the current mailbox operation, within the
  existing 4096-ID transient deferral budget. This lets later pages supply its
  parents; a subsequent drain retries the child through normal admission. The
  transient deferral does not enter the node-wide seen cache or change wire data.

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
On reconnect, a recipient's recurring DM catch-up only visits contacts whose
device fingerprint the user explicitly verified against the safety number.
It first reconciles directly with that sender. If that round cannot complete,
it tries the elected post office; desktop and CLI hosts also drain verified DM
conversations from the post office periodically. The desktop trust file restores
this allowlist at login; the CLI persists explicit `/verify-peer` decisions.
Discovery or a valid handshake alone never authorizes recipient-initiated DM
pulls. Unverified peers can still push messages directly.
An explicit, default-off bridge-discovery preference is a separate authorization:
while a public node participates in a bridge-enabled post office's authenticated
directory, its fresh, self-signed introductions can be polled from that post
office's opaque mailboxes. This does not mark safety numbers verified, persists
no bridge permission, and expires with the directory membership. Invisible mode
does not participate. See [routed network discovery](routed-network-discovery.md).
For multi-site federation, post offices have equal roles. Operators can use a
shared set of identity-pinned seeds or an approved network authority with
per-device expiring memberships and a public revocation policy. Network mode
checks both memberships against authenticated Noise identities before disclosing
inventories/presence and never downgrades to pin-only federation on failure.
It automatically discovers LAN members; optional pinned seeds locate routed
members. The offline authority key stays off relays; policy changes require
distribution and restart. Membership is operator approval, not software integrity.
See [network administration](post-office-federation.md).
Both modes exchange short-lived signed opt-in presence and reconcile version-3
pairwise opaque mailboxes over Noise. The original mailbox arrival time follows
each federated copy, preserving the 30-day maximum. Clients can prefer a local
relay and fall back to another reachable relay. New clients also read and write
legacy relay-specific mailboxes during migration. A legacy `--bridge-peer`
gateway remains available for old clients. Cross-site continuity requires a
physical routed path through at least one reachable relay chain.
The sender's retry worker independently attempts direct delivery before relay
replication, so either side can initiate recovery when a route returns.

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
while text and legacy ID-only stickers remain ordinary message events. New
[sticker media](CUSTOMIZATION_PACKS.md#sticker-message-delivery) uses the same
encrypted media manifest and chunk path, with bounded presentation metadata in
the filename so deployed media-capable clients still show the image. Same-host,
same-deadline verified transfer reuse does not share keys between conversations.
A current sender publishes the
manifest first, then asks the exact authenticated recipient device for consent before
synchronizing chunk events. When the clients cannot connect directly, the recipient
returns a signed decision through the existing recipient-encrypted pairwise mailbox;
trusted relay federation carries it without opening it. The decision binds the original
manifest event, file conversation, complete device identities and known accounts.
Direct sessions query the same signed control as mailbox delivery using appended,
optional sync frames; existing frame discriminants remain unchanged. Monotonic
recipient revisions prevent a delayed acceptance from replacing a newer decline or
pending decision. Every successful receive-policy write invalidates cached publication,
even if a decline and reapproval occur between mailbox rounds; reopening refreshes the
revision. Legacy direct replies have no ordering token: their pending/declined answers
remain authoritative until a fresh direct answer, rather than allowing an unseen delayed
mailbox approval to override them. Invalid controls or failed consent persistence fail
closed without entering the legacy automatic-transfer path. Verified durable saves add the exact final chunk to a
completion control, allowing the sender to finish delivery through the same path.
Relay custody remains pending. The receiver independently rejects unsolicited chunks at its
durable sync gate, including chunk-first delivery from older senders. A channel sender
publishes the manifest and members pull chunks only after their own consent; automatic
images retain eager distribution. Decisions bind the signed manifest event and survive
restart. Local receive policy remains in `attachment-consent.log`; signed protocol
controls and authenticated direct observations use the separate encrypted
`file-consent-controls.log`. `file_consent` owns control validation and exchange,
`file_consent_store` owns durable revision and replay state, and `FileBook` indexes
offers by author. Exact manifest lookup avoids sorting all aliases for each offer;
publication resolves alias policy once per file and excludes channel manifests. Saved
completion controls require the current owner account, original sender and matching
sender certificate. A failed control write is retried without blocking unrelated mailbox
messages. Identical signed manifests that share the same file conversation are aliases
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
the active local user with the Mesh-Talk password on non-macOS platforms;
macOS uses system device-owner authentication with Touch ID or the Mac login
password. The same verifier serves individual protected-message reveals.
Frontend ownership generations discard stale
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
network awaits. Control exchanges run in a worker-owned task set with at most
two destination retries to distinct devices, one receipt retry and one peer
reconciliation in flight. Multiple messages for the same stalled device share
one destination lane, leaving the other lane available to a reachable device.
Each complete direct or relay exchange adapts within a 15–60 second deadline so routed
round trips can finish without blocking another lane or file scheduling. Active
immutable event IDs prevent duplicate scheduling within a lane. Busy lanes leave
their pending cursors unchanged so waiting destinations, receipts and peers retain
their next scheduling turn. Dropping the
accept loop cancels its worker and aborts the worker's control tasks, releasing
channels and their admission permits. File phases run in independently owned
workers with separate network deadlines below.
Historical DM and control pulls for manually verified contacts use a
receive-only reconciliation projection; DM bodies prefer the direct sender
route and fall back to the relay, while controls can be pulled directly or
through the relay when the original sender is offline.

Immutable file retries use eight scheduling slots and retain at most six
authenticated connections across complete protocol exchanges, with a ten-second
idle limit. One outbound connection permit remains available for control work. A rotating admission
cursor fills available slots; at most eight independent workers advance file
steps while ordinary control work continues. Clean progress schedules the next
step; waiting consent releases its socket and slot until a coalesced wake or
bounded retry becomes due. Fresh work tries the direct recipient first;
failed routes discard their wire state before advancing to another eligible
candidate. Exhausted candidates free the slot rather than busy retrying.
Dialing, fingerprinting, reconciliation and custody checking each have their
own clean exchange boundary under a 15-second network operation deadline. Each step
rechecks the current private authorization and exact destination binding; a
timeout discards the connection and any partial frame, resuming from durable
remote state on a fresh connection. Only final, target-qualified chunk custody
retires source file work.

Relay file transfers use the same retained scheduler. Legacy and federated
manifest/chunk mailboxes advance through separate clean exchanges, reusing
`opaque_relay.rs` for encryption, projection and received-event admission.
Every upload step rechecks the exact offer consent, destination binding and the
selected relay's current role, identity/account and send policy. Interrupted
streams and their partial synchronization state are discarded together.
Only identifiers and protocol progress survive an exchange; ciphertext payload
projections are rebuilt temporarily, trading repeated local wrapping work for
bounded retained memory. Do not combine these exchanges into one whole-file
deadline: repeated cancellation before the first chunk can starve delivery.

Relay custody releases the connection and active slot but never retires the
recipient's file destination or confirms delivery. At most 256 volatile,
ten-second scheduling hints defer redundant relay uploads; eviction permits an
earlier retry and carries no custody guarantee. The SDK's immediate best-effort
flush remains a disposable, bounded attempt rather than retaining worker-owned
state. SDK hosts run the accept loop's delivery worker for eventual retries.

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

Equal-relay enrollment uses the optional, authoritative versioned public
`--federation-config` manifest; repeated seed flags remain compatible when no
file is supplied. Atomic operator mutations serialize through a persistent
companion lock. Running services use a startup snapshot and require restart
for enrollment/revocation. See [federation operations](post-office-federation.md).
Modern directory v3/v4 and federation v2 admit up to 1,024 presence entries, while legacy
requests keep 128. Client refresh streams all eligible relays with eight queries
in flight and publishes each persisted result immediately; policy generations
and staged rollback prevent canceled or obsolete introductions from surviving.

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
Owner-scoped channel text, sticker and file enqueue commands return exact durable
IDs before network work; a separate admitted fanout retains runtime retirement
protection. Nickname publication uses shared runtime admission and synchronized
metadata, leaving lifecycle replacement exclusive. The authenticated account profile owns
nickname, bio and avatar across linked devices; desktop projection persists the
nickname and emits an owner-scoped update, while startup/roster identity reads
repair a missed frontend event. Legacy source migration is marked per local
profile/account and never seeds a joining device's previous nickname.
`NodeRuntime::display_name`
returns an owned snapshot so concurrent rename never exposes a borrowed mutable
name. Member and verified opaque post-office custody are separate projections,
with post-office confirmations never retiring member-copy recovery work.
`store/chat.ts` exposes the UI-facing state and routes incoming DMs to the sender's
_account_ (one conversation per multi-device contact). `features/chat/` is a two-pane app (conversation list ·
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
`chat_commands/presence.rs` projects eligible observations by verified account,
retaining devices, source and expiry independently of routes and connection state.
The isolated `store/presence.ts` ages those lifetimes during silence or IPC errors,
protects owner/runtime generations and response ordering, and retains bounded
session-local last-seen history. Online/Offline are client-relative observations;
recent history remains secondary to Offline. A global notice reports failed
presence refresh or terminated discovery producers without asserting an Internet
or LAN outage. Public device observations have a 15-second presence lease, separate
from retained dial routes. Local and routed refreshes and directory registration
run often enough for pushed snapshots and shared two-second local evidence aging
to show a stopped device Offline within 20 seconds. Older hosts retain polling. Private authenticated routing does not contribute.
Directory v4 preserves the
origin expiry across the final relay-to-client boundary; older directory replies
remain route hints without eligible freshness. Routed inventories refresh every
five seconds with 20 ms candidate pacing; local fallback scans use paced rotating
slices with capped jittered backoff, while bridge registration retains lease refresh. Wire layouts and admission limits
remain unchanged.

Conversation headers show accepted distinct account counts and known-online
counts, including self while an accepted local node runs. Pending/removed members
and infrastructure are excluded. Account-unresolved device memberships appear
separately rather than being labeled people. Account-authorized channels apply
membership and owner roles to accounts; certified devices inherit those roles
without separate owner approval. Legacy device authority remains until a verified
owner-signed migration. Member profiles expose deduplicated fresh devices and overlapping source
details without inferring physical LAN location or claiming a connection. Group
history through an intermediary does not propagate presence. See the
[network presence contract](network-design-reference.md#person-presence-contract).

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
in `accounts/<local-user-id>/hidden-contacts.json`. Offline contact deletion uses a
separate device-local `deleted` account-ID list in the same atomic snapshot
(defaulting to empty for existing files). Roster reads suppress these accounts until
fresh online presence clears the deletion. The delete command and roster read
serialize policy, session, and runtime checks in that order. Deletion reuses
`Node::clear_account_conversation` to erase local history and cancel pending delivery
before saving the contact policy; cryptographic identity proofs remain intact.
The history stores and contact policy have separate atomic commits. A failure may
leave history partially erased, so the UI reports failure and allows retry. Mutations validate the session owner
and replace the file atomically. A linked device may optionally synchronize these
presentation choices through the account preference log; each device keeps its
own contact-sync switch.
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
in [platform validation](platform-validation.md).

### Initial native presentation

The application root starts the existing authenticated runtime and account-scoped
appearance coordinators before loading the chat view. Startup and chat share
wallpaper composition; synchronous owner, account, runtime-session and lock checks
conceal retired images before asynchronous cleanup.

The native main window starts hidden. Its visibility owner accepts one renderer
acknowledgement after a safe entry surface or verified local appearance and decoded
selected image have a committed layout. The pack owner publishes selected-theme
preparation under its current scope/session, so unrelated library enumeration
does not hold the initial window. Full-library completion remains a storage
fallback when point lookup is unavailable or fails. This preparation does
not await full node readiness or remote peers. Hidden webviews use a layout/timer
boundary because animation frames may be suspended before mapping.

The same native owner supplies a ten-second fault fallback for normal launches.
Both paths claim the pending show on the native main thread. Explicit visibility
and close/minimize choices retire it, preventing delayed focus or redisplay.
Hidden autostart never arms an automatic show. The window-state plugin restores
geometry and decorations without restoring visibility. Presentation IPC is
allowed for the locked entry screen, targets only the main window, and admits no
account data or runtime operation.

Explicit main-window restoration is owned by `window_visibility`: show through
the existing placement-preserving path, unminimize, then focus. The macOS
application `Reopen` event (including Dock activation), single-instance activation
and tray Open/navigation actions share this operation. A tray click restores a
hidden or minimized window and hides a visible, non-minimized window. Close to
tray continues receiving in the background; it does not minimize or quit.

On macOS, run `cargo run -p mesh-talk --example native-window-reopen-eval --
/absolute/disposable/namespace/data --hidden` to check production close handling,
AppKit reopen dispatch, repeated restoration, minimized restoration and focus.
The probe uses isolated app data and exits automatically. Also verify physical
Dock and menu bar clicks in a packaged build; delegate dispatch does not certify
those system input paths. Preserve locked-entry behavior and hidden autostart.

### Recent search references

The opt-in core `Node::search_recent` query returns `RecentSearchResults`: matches
with a `SearchReference` and an explicit `per_conversation_limit` of 2,000.
Eligibility reuses the existing search owner: available human device proofs,
durable account conversations, and locally available channels; relay-only
accounts and current relay devices remain excluded. The desktop hidden-contact
preference remains a separate presentation filter.

Each reference names its conversation kind and canonical message ID. Account
references use the envelope's logical ID and account-history deduplication;
channel references use event IDs, including original IDs preserved by recovery;
legacy device references stay device-scoped even when a verified account binding
is available for filtering. Unresolved zero-sentinel IDs become `None`, never an
exact target. Display text, sender and timestamp cannot substitute for identity.
A reference carries no read authority: future context/IPC consumers must recheck
the current owner, conversation access and expiry/recall policy at read time.

The query searches the latest 2,000 canonical history entries per conversation,
using account/device history order and channel causal order to select that window.
Files and recall tombstones occupy window slots; only current text is matched,
case-insensitively. Accepted pending transactions and sibling-copy deduplication
follow the owning history projection. Returned matches sort newest wall-clock
first; ties within a conversation retain reverse canonical history order.
Missing matches do not establish absence from all retained or remote history.
Canonical history scans borrowed retained metadata and copies message content
only for the selected window. Identity deduplication and channel causal ordering
still require metadata proportional to retention; this is not a constant-cost,
paged or complete retained-history search.

`Node::search_context` resolves an exact reference through the same conversation
eligibility and canonical history owners. Missing, zero, expired or recalled
targets are unavailable. A found context contains the target and up to 25
neighbors on each side in canonical order, plus the number of entries after the
target. Recalled neighbors retain only their tombstone; their re-edit plaintext
is omitted. Context reads do not change cache coverage, unread state or delivery.
The output window contains at most 51 decoded history rows. Retained identity
selection and ordering have the same metadata traversal cost as recent search.

Desktop `owner_search_recent` and `owner_search_context` capture explicit session
ownership before queued work and recheck it before publication. References are
validated at the host boundary: account/device targets are 32 hex characters,
channel and message IDs are 64; invalid shapes have structured input errors.
Queries accept at most 4,096 UTF-8 bytes. Search returns a newest-first prefix
with at most 200 hits and 1 MiB of serialized output, setting `truncated` when
either result budget omits matches. The desktop uses
`Node::search_recent_limited` with a 201-hit budget so one extra match establishes
truncation without accumulating every conversation's matching plaintext. The
current conversation's 2,000-entry projection is processed separately; retained
metadata scans still cover eligible conversations. Context returns at most 51 rows and 1 MiB;
a response exceeding its byte budget becomes unavailable with no rows. Limits
do not establish complete retained-history coverage. Keys, raw errors and
message content do not enter diagnostics.

The desktop SearchDialog uses these owner-scoped commands through typed adapters.
Each selected result retains its namespace and canonical message ID. Exact
context has a separate presentation lifetime; it does not replace the live
conversation cache or commit reception/unread coverage. Returning to the latest
messages restores the live view. Unavailable or unresolved references produce an
explicit explanation rather than matching text and timestamps. A legacy-device
reference needs an explicit account association before opening an account view;
the raw device ID is never substituted for an account ID. Search results expose
their recent-window coverage and truncation. Owner/runtime replacement and
superseded requests invalidate pending results and context.

### Storage replay and query budgets

The indexed-storage connection owner in `storage/database.rs` uses SQLCipher
for encrypted and authenticated database pages, including indexes and WAL data.
It verifies the application/schema version and encrypted owner binding before
use, and never replaces unreadable storage with an empty database. A profile
shares one serialized connection; domain stores own their tables and bounded
queries. SQLCipher passphrases are domain-separated so user input cannot select
the raw-key syntax. Production key derivation retains a 600,000-iteration
password work factor; only existing test configurations reduce it.

Event repository schema v2 admits the signed `ChannelMetadata` event added by
channel protocol v5. The only v1 schema change relaxes the event-kind constraint
from 18 to 19. Upgrade validates the exact owned v1 tables and indexes, then
changes that fixed constraint and version atomically using SQLite's documented
schema-only procedure. It preserves retained payload pages and foreign keys;
interruption rolls back the version and definition together. Unknown or weakened
schemas fail explicitly. Capability probes use a covering kind index and never
load retained bodies; a newly required v5 control invalidates an older sync view.

Fresh encrypted databases establish incremental auto-vacuum (mode 2) before the
first metadata table. Warm open authenticates application/schema and owner first,
then requires the same mode. Unsupported NONE/FULL modes fail without a full
VACUUM, initialization over existing authority or an empty-store fallback.
Authenticated legacy publication creates its new database with this policy.

Connection policy disables mmap, keeps temporary data in memory, suggests a
4 MiB page cache, and uses FULL synchronization with a 256-page automatic WAL
checkpoint trigger and a 4 MiB retained-journal target. A connection also retains
at most 64 prepared SQL statements. Relay inventory, session authority, maintenance
and network metadata selectors share this fixed cache; the former 16-entry bound
recompiled warm queries as these paths displaced each other. Reused statements
still execute every eligibility, readiness, expiry, revision, session and lifecycle
check against the current transaction. Query results are
not cached, and returned statements clear their bound values. These are connection budgets,
not a global RAM or disk cap: query results, concurrent domain work, long readers
and historical data require their own bounds. Failed commits, failed rollbacks
and fatal storage errors invalidate the handle until reopening. Read failures
remain errors rather than missing records. The connection owner does not by
itself convert legacy event or transcript stores to demand loading.

Physical maintenance reads checked page-size, page-count and freelist scalars.
Its explicit budget accepts one through 64 free-page iterations, default 64.
The row-producing incremental-vacuum pragma is stepped through completion inside
the existing transaction; even zero-column result rows represent engine work.
Progress is returned only after COMMIT. Zero free pages avoid a vacuum statement
and write transaction; late statement, COMMIT and rollback failures retain the
same authority and fault policy. No whole-file scan or history-body hydration is
needed to discover this work.

The dedicated worker starts a physical drain at 256 free pages, stops at or below
64, and processes at most 64 iterations per eligible slice with a 100 ms success
cooldown plus its measured-duty gate. These are internal scheduling/hysteresis
choices, not physical-byte, latency or throughput guarantees. The class has
independent 100 ms–5 s retry eligibility for typed BUSY/LOCKED and incomplete
checkpoint progress; other role, digest, staging, archive and session work
continues. With only future physical work pending, the worker waits for its
eligibility deadline, cancellation or a normal wake instead of polling a backlog.

Each successful vacuum slice requests one background PASSIVE checkpoint outside
its transaction and live statements. A partial frame transfer remains pending
even when SQLite returns a zero busy flag. On the first worker inspection only,
one known main-file length comparison against the checked logical page estimate
discovers pending truncation after restart, including below-threshold freelists.
Ordinary WAL growth does not initiate this completion path. No-WAL or complete
frame progress clears the intent without promising shrink of mere file padding.
A pinned reader can defer completion; retries never force WAL truncation or a
checkpoint loop. PASSIVE may copy an arbitrarily large available WAL backlog,
so its engine work is **not** capped by the 64 vacuum iterations. The measured
slice duty and cooperative shutdown include that admitted synchronous work.

Logical domain charges, reusable free pages, the SQL page estimate, main/WAL file
lengths and actual filesystem block release are separate observations. Completed
checkpointing permits main-file truncation while the owner remains open; allocated
WAL space may remain for reuse. Neither the page budget nor the WAL trigger is a
hard total disk or I/O cap. Physical maintenance removes only already-free pages
and does not change live signed events, counters, local suppression or retention.

The shared core profile lease retains the operating-system lock on persistent
`node.lock`. CLI, central participant startup and offline recovery acquire it
before opening keystores; public Node constructors acquire it for their
caller-owned identities. Handed ownership moves into the context once and lasts
through all retained stores, admitted work and retirement. Acquisition rejects
unsafe aliases, unrecognized nonempty lock data and live legacy PID holders
before mutation. Empty and stale numeric diagnostics retain compatibility.
Release clears the PID while holding the lock and never unlinks its path.
Canonical root aliases and same-process callers cannot create a second cooperating
owner. Standalone legacy store APIs do not acquire a profile lease implicitly.

Exact event and sent locators remain explicit. Bare filenames normalize their
empty parent to the current directory; safe Unicode leaves are preserved.
Reserved source namespaces, case aliases and existing file identities are
checked before conflicting writers open. An explicit external sent parent has
its own immediate, nonwaiting guard under the same owner; no ancestor directory
or cryptographic account is inferred from a filename. Internal source locators
remain relative; external sources require their explicit mappings on relocation.

Database relocation requires explicit checked retirement: truncate-checkpoint
the WAL, reject busy or incomplete results, and check connection close before
moving files. Failure preserves the database for recovery rather than allowing
publication of a main file without committed WAL data. Ordinary append and
normal open do not perform this retirement checkpoint.

The shared profile context retains that lease for every repository using its
single encrypted connection. Its immutable owner binding includes both device
public keys and a versioned profile class, separately from mutable account
membership. Explicit role and source-domain manifests prevent filename-based
ownership inference. Source locators inside the profile are relative to its
leased root, preserving closed-profile relocation.

Participant control metadata uses that same physical device binding and an
authenticated participant role, exact source layout and active/pending account
identity. A relay-only authority cannot be reinterpreted as a participant.
For an absent main database, the lease-backed context first runs the actual
legacy Node validation. Only successful validation permits authenticated private
candidate publication and connection initialization. A wrong password, unrelated
account or corrupt legacy source cannot publish conflicting authority; header-only
legacy sources retain their existing semantics. Existing unreadable authority
or orphan canonical WAL/SHM never selects fresh initialization.

Account adoption extends the delivery owner: durable delivery and SQL intent
precede account-keystore replacement. An ambiguous save reloads the authenticated
key under ownership. The exact old key cancels, the exact prepared next key
completes, and any third key or unreadable result fails closed. Recovery is
idempotent across delivery binding and SQL completion. A pre-SQL delivery intent
can preserve its exact previous account ID through bootstrap without inventing
an unavailable full public key. Old history retains its original provenance.
Security rekey does not manufacture a signed linking-continuity proof.

Participant events now use the same encrypted profile connection and a distinct
event domain. The participant constructor authenticates its existing legacy
owners before first SQL publication, then completes event-source publication
before delivery-journal recovery. The journal keeps its established crypto,
event, transcript, receipt and outbox recovery ordering while unrelated sources
retain their legacy owners; no multi-domain atomic cutover is required.

The event source has its own persisted publication manifest. Authentication uses
a private copy and the streaming legacy visitor, preserving the original source
on failure. A manifest-bound `MTSDB1` downgrade barrier freezes the source before
verified staged events and published authority commit together. Interrupted
preparation, freeze, import and publication resume under the held profile lease.
Warm opens inspect only the manifest and exact small barrier; they do not hash
or reopen legacy history. Archive removal follows destination checkpointing,
and durable cleanup flags retain retry work for the runtime maintenance owner.
A missing or changed barrier is an error, never an empty-store fallback.

`PersistentEventLog::open_profile` is the production Node path. Its indexed
backend retains no event-body cache; exact reads fetch complete owned events
from SQLCipher, and ordinary appends leave the retired legacy source untouched.
The explicit standalone `PersistentEventLog::open` remains the collecting legacy
SDK owner and rejects a retired participant source. Transcripts, crypto and
projections still have separate migration boundaries. Transcript history ordering
and account routes use persisted metadata selectors; ordinary recent and
exact-context reads hydrate selected rows only. Combined-order input checkpoints
seek after the prior transcript ordinal on ordinary appends and reuse the signed
order owner’s safe-tail generation. Rewrites, deletion, due expiry, late roots and
identity changes invalidate this prefix; interrupted retirement or extension
cannot publish partial ranks. Unsafe changes retain the synchronous metadata
rebuild, and file-eligibility revisions currently invalidate conservatively.
Production network authorization
prepares finite SQL selections through yielded header/edge turns. Individual
channel-control authority queries and the explicit collecting SDK compatibility
adapter remain separate; these cuts do not establish the complete participant
runtime work bound.

The channel checkpoint owner reuses the existing canonical logical-order job and
its source revision. Safe tails extend the same job; a late control that changes
canonical order rebuilds derived authority in bounded turns. Current metadata,
selected causal membership, activity rows and exact applied identities live in
indexed tables. Retired derived generations are reclaimed incrementally. A warm
ordinary Node open loads current channel metadata without decoding completed
signed channel history. Source 8 independently publishes legacy channel sender
snapshots; only each channel's active sending epoch is loaded into memory.

Receiver state is separate from rebuildable authority. One exact channel, author
and epoch checkpoint preserves consumed holes, skipped keys and their eviction
order. Preparation never mutates it. The account/source-bound compare-and-swap,
received body or exact suppression evidence, reception provenance and batch
progress commit in one SQL transaction before any in-memory publication. Failed
writes leave the key usable. Replaying old distributions or reordered controls
cannot roll consumed keys back. The owner provides separate message and file
batch terminal facts. Channel file reception installs the receiver, FILES row,
validated FileBook offer, reception fact and file-role batch progress in that
same transaction. Its existing group-completion reservation precedes the
transaction; reservation failure leaves the ciphertext and consuming key pending.

File provenance counts surfaced valid cards, excluding malformed, expired,
suppressed and conflicting transfer bindings. One bounded continuation retains a
first candidate and a private receiver checkpoint while it crosses rejected
plaintext. Existing pending rows hold its speculative progress; at most 64 bodies
and 64 selector turns share a 16 MiB charge budget. Source changes discard the
private keys, and restart or resolution resets speculative marks in bounded pages.
Authenticated rejected plaintext is consumed with exact evidence. Production hosts
receive only durable facts and coalesced wakes; the explicit legacy SDK retains
one admitted manifest for its final callback and rechecks visibility before delivery.

Pending channel work is selected by bounded indexed pages. One message producer
invocation examines at most 64 bodies and 64 selector pages, sharing a 16 MiB
charge budget across its previews while retaining the existing 8 MiB per-row
admission. Two successful candidates distinguish catch-up from live delivery.
When the budget ends first, a lone candidate stays pending without consuming its
key; a scheduled continuation preserves arrival provenance and leaves the batch
terminal open. Startup continuations also retain their expiry and notification
semantics. Authentication failures wait for changed key material; sequence gaps wait until receiver
progress can satisfy the existing skip bound. Valid oversized records remain
pending with an explicit budget error. Definitive malformed sender headers
commit exact general-discovery evidence with their channel disposition. Missing
recall targets wake exact persisted dependencies. Invitation history uses indexed
offer/release selectors, canonical and causal authority, and the existing bounded
frozen text copy; unchanged retry ticks do not replay historical control bodies.
Shared delivery journals retain their existing last-retired recovery ordering.

The separate `node/transcript_repository/` owner provides indexed sent, received,
file-card and exact suppression storage on that same encrypted connection.
Signed file proofs and logical-file bindings reuse its per-source publication
barrier. Proof reads validate row and physical allocation charges before payload
fetches; aliases remain bounded by the signed identity limit. Persisted revisions
advance only for newly accepted proofs or bindings, so duplicate insertion and
restart preserve ordered-view invalidation. Export visitors release the SQL guard
between bounded pages. Production Node uses the indexed FileBook described below;
other independent runtime owners retain their own resource and startup contracts.
The indexed FileBook projection references those existing file-card bodies by
ordinal. It installs validated signed bindings and exact processing facts in the
body transaction. Its transfer fingerprint shares the existing file equality
rule: presentation names and MIME can differ between aliases, while version,
protection and immutable transfer fields remain bound. Each source row still
retains its exact plaintext digest. Physical deletion cascades to eligibility metadata, advances
its persisted revision and retains compact processing evidence. Durable local
suppression also revokes eligibility in that same transaction, before physical
cleanup can fail. Signed logical aliases apply this rule in either arrival order;
the indexed author and exact plaintext digest must match the retained offer.
History and sibling archive selectors qualify FILES ordinals through this validated
projection before reading bodies, and bind their cached generations to its persisted
eligibility revision. Selected reads recheck eligibility before hydration. Archive
retirement clears its ready marker before deleting the first bounded batch, so an
interrupted rebuild cannot serve a partially retired generation.
Metadata pages check row and byte budgets; selected manifest reads verify physical allocation
charges before loading bytes. Account adoption fences old handles and never
imports the former account into the new namespace. Production Node imports old
FILES bindings once through bounded turns; warm reopen reads checkpoints and exact
retained erasure keys without hydrating file-card history. Download scheduling is
account-scoped SQL work installed with each new validated offer. Metadata discovery
pages contain at most 128 IDs and each worker turn admits eight files. A completion
acknowledges the observed work revision, so a newer offer cannot lose its sole retry
fact. Manifest receive discovery scans at most 128 committed headers per turn;
only commits owned by the captured account enter that account's pending index.
Returning to an account resumes both undiscovered and pending work without admitting
intervening accounts' events. Adoption generations reject old handles and ACKs even
when the same account returns. Rows are admitted by count and decoded allocation
cost before body reads; an oversized legal row remains pending for a larger budget.
The direct-file producer and idle retry worker use this index; channel FILES use
the independent checkpoint/CAS scheduler and rotate a bounded discovery prefix
so a stalled channel cannot starve another conversation. Consent changes restart
a persisted metadata scan, and failed media or
completion writes leave pending work retryable across restart. Sent fanout scopes
reuse source 9 publication and bounded metadata pages; the sync owner still checks
each exact signed parent permission. Ordinary scope reads require a live validated
binding, and historical scopes are never rebuilt in a resident map. Delivery-journal file
projections now install FILES, validated eligibility and incoming reception facts
in one transaction before retiring the intent. The same installer accepts the
incoming channel owner's receiver checkpoint transaction. Outgoing manifest
projections create no incoming effect. Signed same-account certification retains
own-device attribution across recovery; retry preserves the first durable effect.

The attachment receive-policy owner uses source 17 of the shared publisher and
account-qualified SQL facts. Global automatic receive defaults to false; exact
latest decisions across validated aliases override contact/global preferences.
The sequence and policy generation commit with each decision, so failed writes
cannot advance scheduling or supersede a durable decline. Warm open reads no
historical decision frames or maps. Adoption starts a fresh policy namespace and
preserves the prior account for a later return; generation fences reject old
handles even after that return. Initial legacy records belong to the authenticated
original or bootstrap-previous account. An unpublished experimental legacy source
with multiple account namespaces and no authoritative attribution fails explicitly
and preserves its bytes. Existing signed remote file-consent revisions keep their
independent ordering and admission contract.

`node/reception_effects/` provides the derived reception-work owner for bounded
host integration. An effect references its existing transcript row and stores
first-installation provenance, own-device and batch metadata without copying its
body. Producers install it inside the same SQL transaction as plaintext and
exact processing evidence, before completing the existing delivery journal or
channel checkpoint. A separate terminal batch fact survives deletion of its
former content template. Legacy historical-source migration creates no effects.
Each receipt stream, exact installation identity, and fixed consumer cursor is
scoped to the authenticated `participant_profile.account` key. Producers derive
that key within their existing SQL transaction and verify the producer's captured
participant account and adoption generation there before installing effects.
Message and file producers are recaptured after adoption publication; a former
account's producer cannot revive when that same account later returns. Migration
handles may import historical bodies but cannot install new reception effects.
Open readers capture the same adoption generation and reject pages, ACKs, and
retirement after account adoption, including a return to the original account.
Reopened readers preserve each account's pending work, consumer progress and stream
epoch. Runtime/session authority additionally fences
individual host installations.

Participant adoption also owns event commit intervals in the same encrypted
database. Half-open ordinal ranges reference retained account namespaces; the
full authenticated account key resolves that reference. Promotion commits the
range boundary, account namespace and monotonic owner generation together. A
generation fences a captured owner even after switching from A to B and back to
A without appending an event. Exact ordinal lookups let derived receive queues
index the account before selecting a bounded attempt page, retaining earlier
accounts' undiscovered and pending work. Event deletion does not erase these
ownership boundaries.

DM discovery captures that generation after adoption completes. Its pending index
qualifies the original account namespace and authoritative ordinal range before
applying row limits, preserving inactive accounts' work without making processing
claims. Private-control migration has a separate cursor per account and selects
only that account's control headers before body admission. Upgrading the older
unscoped derived queue rebuilds metadata in bounded pages and preserves exact
completed-control evidence; warm reopen keeps the existing discovery position.

An authenticated legacy adoption preserves the previous owner's imported prefix;
the event-source publication transaction fixes its end before new-account event
appends. A compatible older indexed profile with one authenticated namespace can
initialize one range. Experimental indexed profiles with several historical
namespaces and no authoritative commit cut are unsupported: open returns an
explicit ownership-boundary error and preserves the data. Namespace membership
alone cannot reconstruct those lost boundaries. Relay-role storage is separate.

Receipt cursors bind a persisted random epoch and local installation sequence;
sender timestamps never order host work. A finite through-cursor prevents
continuous intake from extending one drain indefinitely. Each demand page scans
at most 128 pending slots and admits at most 8 MiB of transcript body/sender
bytes, plus bounded metadata. Before fetching a body it also checks a separate
JSON upper bound: six times the raw byte charge plus fixed per-row/envelope
allowances, with a maximum 64 MiB response budget. A first row that does not fit
fails explicitly. Deleted, suppressed and expired sources advance the cursor
without returning their bodies; recall and final owner/session authority remain
the materializing consumer's responsibility.

Desktop, CLI output and CLI autosave have fixed consumer identities. A committed
prefix ACK records host processing, not peer delivery or user read state. Pending
metadata retires incrementally only through every enrolled consumer's ACK;
no enrolled consumers means retaining work. Compact exact source-to-sequence
evidence survives that retirement, preventing an accepted-journal retry from
creating another unread event. The desktop must atomically persist its existing
reception state and cursor before ACK. CLI autosave must retain durable save work
before ACK, and a bounded in-memory queue does not satisfy that requirement.
The frontend demand pump uses a single in-flight page and a dirty wake bit. The
existing reception owner still decides unread, mention, own-device, history-sync
and batch-terminal behavior. A single local-storage replacement commits its
bounded ledger, unread counts, undiscovered senders, overflow, history coverage,
batch state and exact decimal-string cursor. A persistence failure leaves that
page pending without replaying its already-applied effects; an ambiguous ACK
retries the same token. The snapshot is scoped to local user, account and device;
the first observed prefix is persisted before applying any effects, so older
per-field writes cannot become the recovery baseline after an interrupted page.
current runtime authority fences every page and ACK. Replacing a runtime stops
old pulls, and a new WebView may replace a lost unacknowledged page. A missing or
corrupt previously committed snapshot is an explicit recovery failure, never an
implicit advance past unread work.

`RuntimeStartup::bounded` and `Node::open_bounded_with_profile_lease` select the
additive production delivery mode. They allocate no DM, channel, file or profile
payload queue; durable reception/current-profile commits wake the existing
coalesced state watch. Legacy callback constructors retain their existing
caller-owned collecting contract. Ephemeral call signals use a separate inbox
of at most 16 signals and 256 KiB including sender identifiers; admission fails
explicitly when full, and a too-small demand read leaves the first signal queued.
Call signaling remains best-effort and adds no wire acknowledgement or replay.
Desktop startup, the node CLI and the dedicated integration host select this
mode. The desktop publishes one scoped wake until its pending page is ACKed;
state, channel, profile and file-progress invalidations share that gate. It
retains only page cursor/token metadata and at most 64 current progress hints.
Current profile values occupy at most 16 rows and 1 MiB raw per page; their walk
restarts on a new revision or renderer replacement. Alongside receipt and call
budgets, their conservative serialization charge remains below 64 MiB. Backend current-value payloads use the encrypted participant indexes; Node warm
reconciliation visits only exact pending source changes. Display-name inventories
read bounded header pages and never hydrate avatars. The desktop resolves a
selected current profile under its local-user/runtime admission; explicit clear
and missing profile remain distinct. It never sends a whole-avatar map through
IPC. The received-avatar cache admits at most 1,024 values and 64 MiB of UTF-16
payload charge, preserving the own-account value where it fits. Owner replacement
and newer local edits fence delayed startup reads.
Native alerts start at launch high-water, sample at most eight live observations
per second and intentionally coalesce excess alarms. They never retire durable
unread work or replay old unread backlog as live notifications.

CLI output and autosave enroll before either consumer starts. Output ACKs its
printed prefix independently. Autosave keeps at most 32 pending file cards and
does not ACK that page until each save succeeds or authoritative state says the
file was declined or removed. Storage failures retain the durable prefix for
retry and restart; a save completed before an interrupted ACK may run again.

`SentStore`, `ReceivedStore` and `IndexedAccountSuppression` require completed
participant authority; integration must retain `validate_participant_request`
before invoking their constructors. Node uses these indexed owners; separate
file, channel, account-document and host consumers retain their own resource and
authorization acceptance boundaries.

Each transcript source imports independently from a private encrypted snapshot,
retaining one authenticated record at a time. Original source identity must still
match before committing imported rows and before replacing the legacy path with
its exact downgrade barrier. Tail repair affects only the snapshot. Import
failure preserves the original source; an interrupted committed import resumes
publication without replaying it. The imported rows become available only after
the barrier is durable and SQL publication completes. A published source with a
missing or changed barrier fails closed. Warm constructors inspect fixed metadata
and barriers without decoding historical bodies or opening another connection.
The existing profile lease excludes cooperating writers; identity comparisons
also reject detected source changes during import.

Custody observations reuse this publisher through auxiliary source identity 16.
Positive and negative exact-event/holder facts stay in the encrypted database;
warm participant startup does not replay the custody journal. Migration seeks
32 observations after its compound key and retries storage failures without
inventing an empty result. Auxiliary identities 16–18 have their own registry,
so extending the publisher does not rewrite the installed transcript table.

Raw account-group access observations remain separate from admitted channel
authority: rejected signed controls still contribute to their original sorted-ID
fingerprint. The access owner computes this metadata using bounded header and
parent pages with disk ancestry marks, and caches the completed result against
the raw control revision, destructive revision and canonical generation. Ordinary
safe message appends reuse that observation; late parents or reordered sources
invalidate it even when the signed control set is unchanged. A source whose kind
index is still catching up uses the complete retained revision. The frontier is
capped with an overflow sentinel; an incomplete local frontier cannot establish a complete observation.
This bounds memory and makes unchanged observations reusable; a changed source
can still require metadata work proportional to its retained graph.

Transcript lookups return owned, fallible results. Chronological, reverse and
insertion-order pages select keys and aggregate sender/body bytes before fetching
payloads, enforcing at most 128 rows and 8 MiB. Invalid budgets and oversized first
rows fail before body reads. Exact event/logical-ID and deadline indexes support
existence checks and expiry selection without payload scans. Deadline consumers
continue until an empty page, since the byte budget may shorten a page. Removal
predicates run under the shared SQL lock and must remain pure; body deletion and
physical row/byte counters commit together. Counts include retained aliases and
per-device copies rather than canonical UI message counts. Suppression uses exact
64-byte conversation/ID keys with bounded export pages and no expiry or Bloom
filter. The existing delivery journal remains the cross-store transaction owner.

Metadata-only transcript pages charge 256 bytes per header and expose stable
ordinals for exact owned body lookup. Source revisions persist across reopening;
conversation revisions retain empty tombstones so deletion and recreation cannot
reuse an old revision. File-conversation metadata is an untrusted selector derived
from parsed file cards and grants no authorization. Recovered sent identities use
the same exact conversation/sequence/time/plaintext-digest key on disk, with no
resident history map. `PublishedHistorySource` shares the independent source
registry and downgrade-barrier checks with auxiliary file-proof and file-identity
owners, whose SQL tables and validation remain their own responsibility.

Relay source publication validates independent encrypted legacy archives before
replacing source paths with manifest-bound downgrade barriers. A private database
bootstrap is checkpointed and closed before publication. Prepared, frozen,
importing, verified and published states make interrupted operations explicit;
event and arrival domains become available together only after verification.
Streaming encrypted staging repairs historical child-before-parent ordering
without retaining the graph or bodies in memory. Staging retains one decoded
event and batches encoded rows within 64 records and 1 MiB, including fixed
metadata and parent capacity; one legal oversized record is processed alone.
Each batch uses the existing durable owner transaction. Canonical ranking batches
only an ordered prefix of at most 64 childless ready vertex IDs, stopping
before any parent that could introduce an earlier-priority child. Ranking retains
fixed IDs, but SQLite may read body pages while locating rows in the existing
WITHOUT ROWID staging table; initial migration remains proportional to history. Parent processing retains bounded 128-child pages. Interrupted imports
restart their authenticated stream; the legacy parser does not provide seek
recovery. These batches do not change the schema or publication barriers.
Arrival import preserves last-record precedence and seeds only missing retained
mailboxes with the upgrade time. The same indexed metadata pass assigns distinct
touch ticks and verifies summed event counts and costs against the event domain;
it does not reread event IDs or bodies. Relay role readiness is published in the
transaction that publishes both mandatory sources. Prepared imports can resume
while unready; a published profile with missing or unready role authority fails
closed rather than rebuilding that authority during ordinary open.

Published opens inspect manifests and fixed barriers rather than rehashing or
decrypting archived history. Replaced or missing barriers cannot trigger empty
creation or automatic legacy merging. Published migration archives are retired
through durable cleanup intent after successful publication; a cleanup failure
does not make them an alternate history source. The dedicated post-office CLI
uses the indexed runtime described below. Other participant domains and the
explicit collecting SDK retain their separate migration boundaries.

The domain-owned indexed event repository keeps immutable signed event bytes,
parent edges, author sequences, frontier and conversation metadata in the same
encrypted database transaction. Explicit domain IDs isolate stores sharing a
connection. A component-wide schema marker is checked before event-domain table
adoption; foreign-key enforcement is enabled and verified by the connection
owner. Existing unsigned 64-bit sequence and clock values use ordered eight-byte
big-endian blobs, preserving their full range.

Large retained-event payloads use an ordinary integer-rowid table with an
explicit non-null composite event key; signed identities and foreign-key targets
remain unchanged. Small key indexes locate the payload row, and every scalar
metadata column precedes its final body column. GC obtains admission order from
the covering commit index. This avoids whole overflowing index-record comparison
and trailing-payload traversal during metadata reads. The choice follows
[SQLite's table-layout guidance](https://www.sqlite.org/withoutrowid.html#when_to_use_without_rowid);
engine plans and cold SQLite page-cache regressions enforce the access pattern.
Small key-only tables retain their separate layout decisions.

Indexed individual lookups return complete owned events. Metadata headers omit
bodies and expose parent counts; parent edges have separate bounded pages.
Keyset pages enforce row and byte budgets before selecting bodies, and bind their
cursors to database UUID, domain, conversation, ordering and source revision.
Raw Lamport pages do not claim causal order. Invalid budgets, oversized first rows,
stale cursors, exhausted repository counters and incompatible component schemas
remain explicit errors. Fingerprints preserve the wire's exact
`SHA256(count_le || sorted_event_ids)` construction: an indexed ID scan uses
bounded memory, and its revision-bound cache persists across reopening. The
first hash after changes may still scan all IDs; unchanged probes do not.
The repository backend alone does not switch the existing runtime persistence or
replace its full-history replay; bounded consumers and verified source migration
are required before that cutover.

Canonical signed-event order uses a separate persisted Kahn generation for
Lamport and wall-clock priorities. Encrypted ordinary work tables hold indegrees,
the indexed ready queue, ranks and a partially drained parent's child cursor.
Seed, vertex and edge work is bounded per step; child updates and their cursor
advance commit together. Pending or stale generations cannot masquerade as a
successful empty or current history. Generation cursors bind their source and
ordering, and publication checks the completed source boundary atomically.

A new event can append to an exact generation only when its priority tuple is
greater than the maximum over all prior vertices, including Profile leaves.
The last ranked event and protocol heads are insufficient proofs. A building
generation can retain a closed commit-prefix and append a validated safe tail;
ordinary monotonic writes must not perpetually restart its repair. Unsafe late
insertions invalidate the candidate, and obsolete work is reclaimed in bounded
steps. These signed-event generations do not include archive-only transcript
roots or provide channel-state checkpoints; their owners must integrate those
sources before runtime history/replay cutover.

Indexed maintenance uses metadata rather than loading retained bodies. Domain
and conversation counters track physical event counts and the legacy record-cost
estimate separately from encoded byte lengths. The event repository maintains
these counters together during append, conversation drop and profile-leaf
removal. Relay bookkeeping reuses these authoritative counters rather than
maintaining another count or byte estimate. A conversation drop removes its own
rows transactionally;
an empty revision tombstone prevents an old cursor becoming valid when that
conversation is recreated. Empty tombstones are excluded from discovery.

Repository conversation retirement provides a separate bounded-reclamation
lifecycle. Beginning retirement atomically hides the whole scope and advances
its revision and destructive generation; physical charge remains until rows are
removed. Scope-known ordinary reads and mutations return a local availability
error, while earlier page, order and network views become stale. ID-only reads
can return absence after the physical ID has been removed. Active conversation
listing uses a partial index that excludes retiring scopes, avoiding a scan of
hidden rows before applying a page limit.

Retirement tokens retain the profile lease and bind the database, domain,
conversation, generation and logical revision. Transaction helpers verify the
actual connection identity. Completion is idempotent for the unchanged empty
tombstone; recreation makes the old token stale. Derived order and profile work
cannot republish a retiring scope. Persisted cleanup phases remove auxiliary
rows and individual edges before deleting vertices in reverse admission order,
preserving foreign keys. Partially erased graphs remain inaccessible until
finalization. Each committed step updates physical count and cost exactly once.

Reclamation budgets distinguish auxiliary work, edges, vertex work and estimated
work bytes from reclaimed event cost and deleted serialized bytes. They do not
promise an equivalent reduction in filesystem size. A single oversized stored
row is an explicit atomic-work exception; it is reported separately and consumes
the step. Ordinary reclamation does not decode event bodies or collect history
IDs. Only a transaction-owning wrapper measures commit latency; helpers within a
caller's transaction cannot claim that timing. The indexed relay role composes
these primitives with expiry and capacity policy. The dedicated relay runtime
uses this indexed authority; participant storage retains its separate migration
boundary below.

The indexed relay state retains the existing shared profile connection
and owns first-arrival time, touch ticks, role readiness and retention policy.
Validated admission and arrival/touch changes commit in one transaction through
the event repository. Its new-growth gate runs after signature, causal and
counter validation and before allocating the stored-event encoding; scalar size
preflight preserves permanent counter errors. Input ownership and cryptographic
validation retain their existing per-event allocations. Signed duplicates are authenticated
without invoking the growth gate, adding charge or advancing touch. Watch
notifications follow successful commit only.

Expiry uses a nullable, persisted monotonic cutoff and indexed arrival deadlines.
A mailbox becomes unavailable at thirty days even before physical reclamation;
clock rollback cannot restore eligibility. Time before the first representable
expiry and deadline overflow do not wrap or prematurely expire an arrival at
zero. Unchanged idle checks neither walk history nor update policy. A trusted
origin must still pass the existing expiry and future-skew checks. After clock
rollback, an earlier valid origin on an authenticated duplicate can correct its
arrival below the persisted cutoff and commit an unavailable result. Rejected
new growth does not make that correction.

Policy-bearing exact reads qualify the current mailbox before fetching a body
or returning positive custody. Their leased views bind the database, domain,
conversation, destructive generation, cutoff and first-arrival identity. Changed
bindings become stale before newer reclamation progress is interpreted; ordinary
appends and touches do not change these policy identities. Physical diagnostics
remain distinct from eligible availability. Warm open validates fixed policy
records and exact schema authority, including required unique indexes, without
materializing mailbox history or opening another database connection.

Capacity reuses the event domain's physical count and estimated cost, including
hidden rows awaiting reclamation. Defaults remain 100,000 events plus 5,000 slack
and 512 MiB plus 16 MiB slack. An authenticated new event exceeding either
envelope requests persistent draining and returns local retryable backpressure
without inserting its body or arrival. An event unable to fit even an empty
envelope has a distinct local failure and cannot trigger futile eviction.
Validation and permanent counter failures remain errors rather than capacity
pressure. These local resource outcomes are not peer-event rejections.

Draining continues until both base targets are met. Indexed expiry selection
precedes least-recently-touched capacity selection. At most one selected mailbox
is pending physical reclamation, preventing unchanged physical charge from
causing repeated over-selection. Role pending state and repository retirement
change together; completion removes the matching arrival in the same transaction
as final graph reclamation. Metadata probes, scope selection and graph work have
separate budgets. Physical charge is not an exact live-mailbox total or a promise
of equivalent filesystem shrinkage.

The dedicated `mesh-talk-node --post-office` path transfers its pre-keystore
profile lease into one `IndexedPostOffice` and shares one `IndexedRelayEndpoint`
across plain, named bridge, gateway, pinned federation and approved-network
modes. Normal reopen uses encrypted indexed state without hydrating the legacy
history log. Initial authenticated legacy migration remains proportional to
source data. Participant event publication is independently owned as described
above; relay adoption does not establish desktop startup or total process memory
bounds.

Federation sessions persist bounded remote inventory and the exact local digest
snapshot actually sent. Owner/session identities, monotonic deadlines and
cancellation qualify each operation in its current transaction. Stale sessions
retain their storage charges until bounded cleanup. Inventory preparation keeps
one active digest context and a finite initial scan followed by an indexed commit
tail. Each pass has separate limits of 512 raw producer rows and 512 retained
event IDs hashed; these are not a combined 512-row limit for the whole maintenance
pass or its other bounded work classes. Derived digest writes
wake maintenance separately from externally visible mailbox changes. Ordinary
metadata probes request only scoped fingerprints; a full demand queue leaves an
optional cache miss for a later probe instead of starting global preparation.
Explicit federation demand starts global preparation. Missing or stale derived
state never authorizes a fabricated fingerprint match or positive custody.

One owned maintenance future interleaves retention, demanded digests, publication
cleanup and session cleanup. It subscribes before inspecting work, waits at least
10 ms and three times the measured slice duration between passes, and backs off
retryable failures from 100 ms to five seconds. Self notifications cannot bypass
that pacing. This is a maintenance duty policy, not a total CPU or transaction
latency limit. Idle passes do not scan historical IDs/bodies or rewrite policy.
Expiry waits use checked monotonic conversion with at most a 24-hour recheck
horizon and retain earlier session deadlines. A faulted or poisoned database
stops admission and fails the owned runtime; it is not reused in a retry loop.
A rollback failure remains an authoritative database error requiring reopen and
retains both sanitized operation and rollback diagnostics; COMMIT failure policy
is unchanged.

Archive-only checkpoint contention or filesystem unavailability returns a typed
deferred outcome with its original cause. Its independent 100 ms–5 s retry
schedule does not delay bounded publication staging, role, digest or session
work; unrelated successful slices and watch notifications cannot reset or bypass
that schedule. Repeated identical cleanup diagnostics do not rewrite their
stored value. Initial publication/reopen may leave these explicitly deferred
archives pending. Invalid source/artifact provenance and authoritative database
failures remain errors, and faulted handles are not reused for diagnostic writes.

The CLI owns inbound sessions, outgoing reconciliation, discovery and gateway
proxy tasks. Cooperative shutdown stops admission, cancels and joins those
owners, lets admitted synchronous database work return, then releases profile
ownership. Unix SIGINT/SIGTERM use this path. Windows hard service termination
requires separate crash/reopen evidence. See the
[relay resource bounds](post-office-federation.md#indexed-relay-runtime-and-resource-bounds)
and [data-directory ownership](../specifications/deployment_guide.md#data-directory-ownership).

Profile maintenance records pending conversations on disk and examines candidates
in bounded steps. Its snapshot keeps the newest two unreferenced Profile leaves
per author and every referenced ancestor. Deleting a child does not make its
parent eligible in the same snapshot. The existing minimum of eight candidates
and reclaimed-cost threshold of one eighth of the domain estimate remain in
force. Source changes invalidate an unfinished selection without losing pending
requests. Applying a complete selection is atomic and updates metadata,
fingerprints and ordering authority together; it may perform disk work
proportional to the removed rows, without rewriting unrelated event bodies.
Participant runtime maintenance now drives these bounded repository steps and
retries publication cleanup through the same admitted blocking-work owner.

Participant network sync uses `NetworkSyncStore` directly: retained Have and
export pages bind a finite commit cutoff and destructive generation. Ordinary
new messages do not invalidate the captured membership. Authorization, sharing,
network-cost restrictions, sibling status and device-policy revision are checked
again before handing off prepared data. Pending invitations retain their existing
8 MiB admission budget and remain in Have until admission completes. No full
repository body or ID collection is retained in memory across transport awaits.
`repository/selection.rs` stores disposable authorized IDs, causal ranks and
outbound policy facts in the encrypted profile database. Have uses only identities;
export tests selection before fetching payloads. Sibling category filtering and
share-off ancestry use retained parent edges plus the peer's exact Have set.
Source, roster, proof, receipt-control and channel-state revisions guard the selection, while
privacy, sharing, cost and device-policy changes invalidate stale views. Unchanged
views also check a domain-wide destructive revision, advanced atomically with
logical retirement or destructive rewrites. This invalidates inherited file
authorization when a parent channel retires, even before physical deletion and
without changing the file's own epoch, transcript or scope rows. Unchanged
Have pages and pre-send validation read no historical bodies; each export fetches
only selected payloads once, including retransmissions. Receipt-control bindings
and their process-local revision publish under one index lock, including complete
cancellation replacements and individual delivery-recovery insertions; a privacy
generation bump cannot hide a later control-map retirement. Ordinary source appends
preserve the original finite cutoff; current authority changes invalidate it.

Opening a production view captures context without collecting retained history.
`privacy_sync/principal.rs` advances canonical ordering, policy scope selection,
legacy/modern authorization, exact parent closure and the unchanged wire
fingerprint in separate preparation turns. The session releases its store lock
and yields after each turn, checking cancellation and current authority on resume.
Historical selected IDs, departures and ancestor marks stay in the existing
SQL selection owner; a persisted current reducer/cursor resumes interrupted
preparation without a body cache. Raw kind revisions include rejected controls
and invalidate preparation independently of admitted channel authority. Message
classification reads headers and, only for receipt-shaped payloads, a bounded
incremental BLOB prefix; matching receipts and selected controls still receive
full authentication. Fresh sessions may traverse retained metadata, so they are
not constant-time opens followed by constant-time preparation.

Preparation defaults to 64 rows, 512 parent edges and an 8 MiB body/work charge.
Large legal parent lists resume at an exact edge cursor. An undersized budget
returns an explicit error without discarding the event. Have-dependent export
closure stages the admitted peer Have in bounded batches and resumes its vertex
and edge together; replacing Have also retires old rows incrementally.
Channel checkpoint projection remains the authority for signed controls. Its
individual synchronous ancestry query still needs the preparing-graph integration;
outer preparation turns alone do not establish a complete control-work bound.
The explicit collecting SyncStore compatibility API is separate from this runtime.

Selection lifetimes use the existing eight-connection exchange limit and admitted
remote-Have budget. Each connection may hold its primary selection and one
non-file parent selection for file-scope authorization, giving at most sixteen
active selections. Drop/cancellation retires a selection and deletes at most 512
rows from one working table; warm open and admitted maintenance take the same
bounded cleanup step. A new view drains previous retirement debt through yielded
bounded transactions before adding another selection, preventing cancellation
buildup. An owner nonce identifies abandoned work after restart without replaying it.

Owned individual participant event reads fetch only the selected body from the
encrypted repository. Standalone legacy SDK reads copy from their resident index.
Explicit whole-conversation collectors remain visible at their callers.
Canonical history, search, protected-content, attachment availability and custody
queries propagate these read failures through `Node` and affected `NodeRuntime`
methods. Host IPC preserves its successful payload shapes and uses its existing
error channel for failures. Boolean Favorites eligibility logs failed canonical
reads and denies capture/export eligibility; it does not authorize a snapshot or
substitute a successful empty history. Typed node/runtime wrappers retain their
underlying error sources.
The outgoing journal's existing acceptance boundary remains authoritative: a
later projection/recovery read failure does not revoke durable acceptance or
turn it into a retryable send result. Supervisors log the original failure and
retain queued work for retry without claiming a send or recipient delivery.
Live receive notifications and derived channel-recall persistence remain separate.
A failed recall-target lookup leaves the signed `Delete` event in the log and
records the failure; canonical queries report the read error rather than an empty
recall set. Control-only recall repair currently depends on a later processing
pass or canonical query, without an independent background retry clock.

Sync storage reads and handlers propagate local failures as errors before sending
a successful empty history, fingerprint match or custody claim. Explicit invalid
input and policy rejection remain separate from local persistence failures. A
batch failure preserves its already committed prefix for idempotent retry; it
does not imply transaction rollback across independently committed events.

Encrypted record logs replay one framed record at a time instead of retaining a
second buffer containing the complete encrypted file. The loader validates a
declared frame length against the remaining file bytes before allocating that
frame. It preserves the existing encrypted format, authenticated decode errors,
valid-prefix crash-tail recovery and Windows repair-handle ownership. This bounds
the temporary ciphertext buffer by one record; decoded retained records and
their indexes still consume memory proportional to retained data. Migration
callers can instead use `open_with_visitor` to process each authenticated decoded
record before decoding the next. The visitor owns any retained values or partial
staged effects. A callback error returns no writer and preserves source bytes,
including an unfinished tail; tail repair happens only after complete successful
visitation. The existing collecting API uses this same framing/parser path.

Canonical history queries select lightweight retained metadata before copying
the chosen message content. Ordering, logical-ID deduplication, pending accepted
records, expiry and recall remain owned by the existing history projection.
Recent search and exact context reuse this projection instead of defining a
second set of history rules. Small result windows do not imply constant-time
queries: metadata selection and channel causal ordering can still traverse
retained records. Account recall authorization also scans borrowed rows and
envelope identities instead of cloning retained message bodies. Durable source
rows retain their established sorted-before-dedup precedence even when arrival
order differs from authored time; pending accepted rows follow durable rows.
Signed DAG ancestry is not pruned to satisfy a local memory
limit; authenticated checkpoint compaction remains a separate protocol decision.

Automatic desktop history refreshes share one running native-read bundle and one
coalesced pending invalidation. The running slot remains occupied until every
original history, reaction and context read settles, even after a presentation
deadline or an early rejection. Explicit navigation, search and retry retain their
own admission path and preserve their history-baseline policy. The existing global
64-read admission limit and owner/run fences still apply. A native read that never
settles holds the automatic lane; explicit reads remain available within the global
limit. This bounds burst refresh work without dropping durable messages.
A queued presentation waiter has one shared ten-second deadline, so a stalled
history operation cannot hold the roster-refresh coordinator indefinitely. This
deadline reports incomplete history without clearing the pending invalidation,
advancing its revision, or releasing native admission.

The standalone legacy SDK's profile maintenance preserves referenced ancestors
and the latest two safe profile leaves per author. Pending conversations share one atomic rewrite rather
than rewriting the entire log separately for each conversation. Maintenance
requires at least eight removable leaves whose estimated event bytes reclaim at
least one eighth of the retained log; smaller obsolete profiles wait for a later
amortized batch. Eligibility checks use borrowed metadata, with a ten-second
global scan cooldown. Reclaimable requests below the byte threshold remain
pending so later conversations can contribute to the same batch. Expensive
maintenance rewrite attempts share a
sixty-second global cooldown, including failed attempts; failures preserve all
pending requests and the original durable log for retry. Explicit compaction
uses the same byte-cost test without the background timer. Encrypted formats,
causal closure and latest-profile projection remain unchanged.

### Manual encrypted backup and recovery

`recovery/{snapshot,archive,key}.rs` defines a versioned logical snapshot inside
an authenticated `MTBACK1` stream. Desktop setup generates one 256-bit recovery
key, which the user saves separately and acknowledges once. The desktop host
retains it in password-encrypted local-user storage for later manual exports.
Every archive still has a fresh random salt; older per-export keys remain valid
for their original archives. Archive chunks are bounded, encrypted with AES-256-GCM using an
HKDF-derived key, and closed by a separately keyed authenticated digest. A missing
final seal, wrong key, altered frame, duplicate record, or invalid signed event is
rejected before a restored profile can be published.

The desktop export command takes the same consuming runtime barrier as account
replacement: it stops producers and drains admitted work, opens the existing
profile offline only when no retired owner is available, streams the snapshot to a restrictive sibling temporary file,
verifies it, then atomically publishes it without overwriting an existing file.
`stop_for_storage` hands the joined Node and its existing context to export rather
than reacquiring a live lease. Before any temporary/archive write, the desktop
checks both the originating host local-user label and the canonical profile root
against current authority; equal mesh account keys alone are insufficient.
Raw SDK snapshot export remains caller-serialized.
It restarts the runtime even when export fails or the IPC caller disappears.
The local last-export summary is password encrypted; it contains the path, time,
size, and counts, but never the key.

Restore is available while signed out. Inspection decrypts to private scratch,
checks the full archive and snapshot, and returns only a preview. Installation
creates fresh local auth credentials and a fresh device identity in an unpublished
directory, re-encrypts supported stores under the new password, then verifies both
auth and node stores under staged ownership. Enrollment uses the same standard
`messages.log` source. Before relocation, every staged handle is dropped and the
encrypted connection is explicitly checkpointed and closed; retained handles or
busy/faulted close prevent publication. The lease is released before the move,
including on Windows. Private staging, host namespace serialization and the
publication journal protect this cooperative gap. A publication marker makes the two directory
renames recoverable: startup removes an incomplete pair or retains a completed pair
before auth initializes. Startup also removes orphan staging and decrypted scratch.
The recovered account fingerprint remains the same; the new device fingerprint
differs. Restored channels initially expose retained archives because old sender
keys are excluded. A certified device can inherit an existing account grant from a
reachable accepted sibling in an account-authorized channel, including while the
owner is offline. Legacy device-only channels first require a compatible,
owner-signed migration; restoration does not fabricate that authorization. Peer replicas
can restore signed ciphertext events but cannot give the new device the retired
device's sender chains or decrypt past channel history. A certified linked device
with retained plaintext can share its available archive after channel enrollment;
otherwise a backup containing that plaintext is required. Offline DM peers are
discovered from durable account history.

Snapshots include visible plaintext history, signed event provenance, file cards,
managed media referenced by visible cards, received peer avatars, signed public
account profiles, immutable account creation metadata, legacy compatibility records,
recall marks, and account-owned Favorites with their metadata, notes and independently
retained attachment bytes. Schema 6 adds exact conversation/message local suppression
facts after the schema-5 records, without changing earlier positional tags. Restore
streams a suppression pass before installing any history, so record order cannot
resurrect known erased content. Suppression is local to its recorded host; signed
ancestry and independent Favorites remain separate. Readers preserve schemas 1–5
and strictly distinguish the
two draft schema-4 extension families; mixed or malformed extensions are rejected.
The Favorites owner applies one eligible record projection to archive count, record
encoding and retained-file authorization. Expired and canonical protected Favorite
content is withheld, while removal generations/tombstones remain. Favorite media
has a separate namespace from ordinary media and shares the archive's byte budget.
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

## Local bot and agent integrations

`crates/mesh-talk-integrations` owns a transport-independent service for scoped
text/reply sends, encrypted profile-bound grants and recent metadata-only audit.
The desktop supplies exact-session admission; a dedicated headless host supplies
profile/lifecycle admission. Private Unix sockets or Windows named pipes expose
a closed local API. The MCP adapter in `crates/mesh-talk-mcp` translates four
context/send/status tools using the official SDK. See the
[setup and limits](bot-integrations.md).

Core `node/outgoing_message.rs` accepts retry keys through guarded enqueue.
Bot and desktop sends retain the host's authorization guard through synchronous
acceptance. Duplicate retries return the original acceptance without sending again.
Request mapping and prepared DM/group state share the encrypted delivery WAL.
Appended journal variants preserve old positional tags; a pending new variant
requires a compatible binary for recovery. Group messages, sender distributions
and group file manifests use serialized sender snapshots and durable replay,
so failures cannot reuse an older sending chain. Duplicate requests return the
original message reference; changed content conflicts. Retry records are bounded
by seven days, 100,000 records and existing outbox budgets. Local acceptance is
separate from network distribution and recipient confirmation.

Local endpoint manifests let scoped credentials survive host restarts without
exposing profile keys. Runtime retirement signals graceful service shutdown and
joins nested request tasks before profile reuse. Administrators separately enroll
bot devices through ordinary signed group invitation acceptance. A send-only
client grant does not remove the hosting member's decryption capability. Bot
badges and authenticated delegated-send attribution are later wire extensions;
existing participants see an ordinary signed account/device author.

## 6. Binaries

- `mesh-talk` (`main.rs` → `lib.rs::run_tauri`) — the desktop app.
- `mesh-talk-node` (`bin/mesh-talk-node.rs`) — headless node CLI; `--post-office`
  runs relay mode.
- `mesh-talk-integrations` — dedicated bot host, scoped credential administration,
  local send/status and explicit enrollment CLI.
- `mesh-talk-mcp` — private-client-file MCP stdio adapter.

## 7. Build, test, CI

- **Workspace** (four layered crates): `crates/mesh-talk-core/` — the UI-free protocol
  core / SDK foundation (lib `mesh_talk_core` + the `mesh-talk-node` CLI bin) — and
  `src-tauri/` — the Tauri desktop shell. `crates/mesh-talk-integrations/` owns
  scoped local messaging, and `crates/mesh-talk-mcp/` owns the MCP adapter. Shared
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
  both builds — **mirrors CI** for PR/review validation. The `hooks/pre-commit`
  hook checks staged whitespace and source formatting; focused tests run during
  development. The optional `check-health.sh --fast` runs a partial gate;
  GPG-signed commits are enforced by `hooks/pre-push`.
- **CI** (`.github/workflows/`): `ci.yml` (Linux, macOS, and Windows matrix → the `verify` aggregate
  check, required by branch protection; coverage → Codecov on Linux), `check-health.yml`,
  `gitleaks.yml`, CodeQL, `dependabot-auto-merge.yml` (Dependabot PRs to `dev`, gated by required checks),
  `glib-0.20-watch.yml` (monthly dep watcher).
- **Automated bug-finding** (defence in depth — surfaces issues without anyone looking):
  - _Coverage-guided fuzzing_ (`fuzz/`, `fuzz.yml`, weekly + dispatch) of selected public
    untrusted wire and opened-payload decoders; the `decoder_smoke` test is the always-on
    stable complement. See [`fuzz/README.md`](../fuzz/README.md) for the current targets.
  - _Mutation testing_ (`mutants.yml`, weekly + on PR-diff) — catches weak/missing assertions.
  - _Coverage_ (Codecov), _clippy `-D warnings`_, _cargo-deny_, _cargo-machete_, _CodeQL_, _gitleaks_.
  - _Delivery / AI regression smoke_ (`make eval-smoke`) keeps core smoke examples,
    prompt/agent contracts, local hooks, PR validation, and CI wiring from drifting.
    _Real model-backed AI eval_ (`make ai-eval`, with `AI_EVAL_COMMAND`) scores the stable
    prompt/workflow cases. _Full real smoke_ (`make smoke-full`) runs unit/integration plus
    backend and frontend E2E.
  - _Claude operations_ (`.claude/`): `/bug-hunt` fans out read-only `bug-hunter` subagents
    (adversarial audit, verified findings only) over the core; `code-reviewer` reviews a diff;
    `/e2e` runs the `e2e-runner` over the real multi-process integration suite.
- **Crypto primitives**: ed25519-dalek and x25519-dalek use `rand_core` 0.6
  directly; the desktop shell uses its own `rand` 0.10. Other primitives include
  aes-gcm, snow (Noise), sha2, hkdf, pbkdf2, and bincode.

## 8. Security posture (summary)

Standard primitives + consistent domain separation; AEAD everywhere; PBKDF2-600k at rest;
Noise XX with identity binding; Double-Ratchet + sender-key encryption with
zeroize-on-drop; signed content-addressed log with fork detection; deterministic relay
election; the sync `have` id-set is streamed in chunks so arbitrarily large conversations
reconcile; the relay bounds its storage by event count and 512 MiB of ciphertext
(LRU whole-conversation eviction) plus a 30-day maximum mailbox age, and its serve
loop (round cap + idle timeout). Arrival metadata is committed only after event
validation, before the event is persisted, so rejected uploads cannot grow the
retention sidecar. The log validates once under exclusive access; duplicate
events do not write arrival records. Forward secrecy remains limited by the
initial DM bootstrap and static-recipient group key distribution described in
section 2 and [the security policy](../SECURITY.md).
Known limitations: device linking relies on
the one-time pairing code (no separate SAS UX); backfill history travels as plaintext over
the Noise channel. A new client's relay upload contains a pairwise opaque
mailbox event: original authors, conversation IDs, kinds, timestamps, channel
names, members, and file metadata are recipient-encrypted. The relay still
sees authenticated connection identities, mailbox use, size buckets, and
traffic timing. The mailbox wrapping has no forward secrecy against later
device-key compromise. Previously stored V1 events remain readable until
evicted. Details and operational limits are in
[`post-office-metadata-privacy.md`](post-office-metadata-privacy.md).

## Retired profile and snapshot metadata

Messages have no content-language restrictions. The core does not bundle language
models, classify message text, or persist language-based visibility decisions.
The interface language is a portable account preference when appearance
synchronization is enabled; operating-system locale remains device-local.

Avatar compatibility publications use `MTPF1`; ordinary account profiles publish
`MTP3` first, then the compatibility avatar under the same publication lock. The
avatar event requires the profile event as a causal parent. Frozen `MTC1` cards
remain readable without promoting their manually declared creation dates.
Decoding still authenticates existing
`MTPF2` signatures so retained avatar events and linked devices remain readable;
retired metadata is ignored and never re-emitted as a preference. Snapshot schema 2
retains the positional tags of its two retired metadata records for old-backup
compatibility. Imports validate those records without installing filtering state;
new exports omit them. Existing `language.log` sidecars are not opened or applied.

## Account profiles and creation metadata

Ordinary profile editing owns one nickname, an optional biography and an avatar.
Signing and authenticated replication are automatic. Public introduction,
creation-witness and JSON artifact workflows are retired. Private first-observed
history and device verification remain distinct from public presentation data.
The conversation header and contact details read the same biography; session-bound
caches reject retired results and refresh on profile invalidations.

Account creation metadata is generated with the key and installed atomically in a
strict versioned encrypted keystore. New pairing negotiation transfers it without
changing legacy encodings or history-sharing policy. Snapshot schema 4 appends
creation and current-profile records, validates their consistency and restores
metadata before opening the account. Legacy schemas preserve unknown dates.
Profile edits accept no timestamps, and old self-declared card dates never become
system-recorded creation metadata. Local clocks and account signatures do not
establish independently trusted wall time or human identity.

The [profile and verification guide](peer-trust.md) owns the complete lifecycle,
resource limits, compatibility and delivery invariants.
