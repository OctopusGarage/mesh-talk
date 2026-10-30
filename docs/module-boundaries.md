# Module Ownership and Reuse

This is the implementation ownership map for maintaining Mesh Talk and assigning
parallel work. Use it to locate the owner of a decision before adding another
implementation. Network semantics and configuration belong to the
[communication architecture](network-design-reference.md); this document defines
where those decisions live in code and how to change them safely.

The [canonical glossary](../CONTEXT.md) defines domain concepts;
[DDD conventions](domain-design.md) define modeling terms and identifier mappings.
The layers and owners below are implementation responsibilities, not declarations
that every module or crate is a separate DDD bounded context.

## 1. Dependency direction

| Layer                                       | Owns                                                                                          | Depends on                                              | Must delegate                                                        |
| ------------------------------------------- | --------------------------------------------------------------------------------------------- | ------------------------------------------------------- | -------------------------------------------------------------------- |
| Protocol core: `crates/mesh-talk-core`      | Identity, membership, encryption, admission, persistence, transport and delivery correctness  | Rust libraries and its internal modules                 | Native platform behavior and presentation                            |
| Desktop host: `src-tauri`                   | Session/runtime installation, OS integration, settings persistence, authorized IPC adaptation | Core public exports                                     | Wire validation, cryptographic authority and delivery state machines |
| Typed IPC: `frontend/src/lib/api`           | Command names, payload shapes, typed results                                                  | Shared types, Tauri APIs and sibling adapters           | UI stores, React features and business decisions                     |
| Application stores: `frontend/src/store`    | Reactive state, owner-scoped work, optimistic intents, cache transitions                      | Typed adapters and shared projections                   | Core authority and feature rendering                                 |
| Feature modules: `frontend/src/features`    | User interaction and composition                                                              | Stores, typed adapters, shared models and UI primitives | Reimplementation of identity, policy or delivery rules               |
| UI primitives: `frontend/src/components/ui` | Accessible visual controls                                                                    | React/Radix and styling helpers                         | Session lifecycle and protocol policy                                |

The bot integration service in `crates/mesh-talk-integrations` depends on the core
and host-supplied admission ports. It owns grants, bounded metadata audit and
private IPC and its fixed-route loopback HTTP adapter; `crates/mesh-talk-mcp` owns SDK/stdio translation only. Core
`node/outgoing_message.rs` owns retry-safe event acceptance and sender-state WAL
recovery. Desktop `src-tauri/src/integrations.rs` binds exact-session leases and
graceful listener retirement. `integrations/local_access.rs` owns serialized
settings/startup control and the single desktop service handle;
`integrations/instructions.rs` owns only the marked user-level bootstrap file.
Both transports share one integration service/registry and core sending path.
`http.rs` owns public static discovery, request limits and fixed scoped routes;
request schemas derive from the existing models. The Agent access settings UI uses
the shared save coordinator and an explicit-owner typed adapter. See
[desktop local access](bot-integrations.md#desktop-local-agent-access).

The local access owner persists absolute authorization deadlines and preserves
them across service retries and session/runtime restarts. Only an explicitly
verified enable, duration extension or renewal can extend authority. The shared
integration grant authorizer checks expiry again at core acceptance. The desktop
activity view uses the existing registry's bounded content-free send projection:
read the live service registry while its supervisor exists, and open the saved
registry only after its writer has stopped under the same controller serialization.
The UI never creates a second grant-store owner or attributes a shared credential
to a particular AI application. See [authorization lifetime](bot-integrations.md#lifecycle-and-authority).

The headless CLI uses the core directly. A protocol change should not require
Tauri; a presentation change should not alter the wire protocol. Not every helper
in `lib/` is pure: `theme.ts`, for example, owns reactive appearance effects. Use
specific modules rather than assuming the directory name implies purity.

The existing API facade remains stable. Its settings owner resolver is composed
at the application seam; adapters never import stores. ESLint enforces that rule
for alias imports and relative imports at any nesting depth. The auth store uses
low-level adapters to avoid recreating the facade/settings/auth runtime cycle.

ESLint also rejects static imports of rendering modules (`components/` and
`features/`) from stores and IPC adapters, including type-only imports and nested
relative paths. Shared presence vocabulary lives in `lib/presence.ts`; identity
controls re-export it for compatibility. Stores own evidence expiry and state
transitions; controls own visual mapping. These checks cover direct static
dependencies, not transitive dependencies or dynamic imports.

## 2. Feature owners and shared decisions

Paths below are relative to the repository root. `core/` abbreviates
`crates/mesh-talk-core/src/`; `ui/` abbreviates `frontend/src/`.

| Capability                                     | Authoritative implementation                                                                                                                                                    | Reuse at callers; responsibility kept separate                                                                                                                                                                                         |
| ---------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Participant lifecycle                          | `core/node/participant_runtime.rs`, `core/node/runtime.rs`, `core/bin/mesh-talk-node.rs`                                                                                        | Reuse prepared/running participant ownership for common network tasks. Desktop retains profile/session bootstrap; CLI retains its data paths, REPL and post-office mode. Adopt host producers and blocking file saves into retirement. |
| Device/account identity                        | `core/identity/` (including `account_login.rs`), `core/node/account_devices.rs`, `core/node/peer_directory.rs`                                                                    | Use verified bindings; never infer authority from names, IPs or sidebar entries.                                                                                                                                                                                                          |
| Discovery and reachability                     | `core/discovery/`, `core/node/privacy_routes.rs`, `privacy_routing.rs`                                                                                                          | Transport route verification stays in core; visibility and reachability remain different facts.                                                                                                                                        |
| Desktop VPN inventory                          | `src-tauri/src/routed_discovery.rs`, `tailscale_discovery.rs`, `easytier_discovery.rs`                                                                                          | Enumerate optional existing clients concurrently; bound, interleave and deduplicate candidates. Core retains signed probing, privacy gates and identity/route verification.                                                            |
| Peer presentation and infrastructure filtering | `ui/lib/rosterProjection.ts`, `contactVisibility.ts`, `postOfficeOptions.ts`                                                                                                    | Reuse account/device/role lookups and role-specific selectors. Diagnostics retain infrastructure; people pickers exclude it.                                                                                                           |
| Direct messaging                               | `core/node/dm.rs`, `dm_envelope.rs`, `dm_ratchet.rs`                                                                                                                            | Encrypt/address in core; UI creates an intent and renders its result.                                                                                                                                                                  |
| Channels and authority                         | `core/node/channel.rs`, `channels.rs`, `channel_account_authority.rs`, `channel_account_bootstrap.rs`, `channel_account_maintenance.rs`, `channel_senders.rs`                                                                                                                     | Signed account grants and full device bindings are authoritative; account inheritance, legacy migration and bounded reconciliation stay in their core owners. Do not derive membership or permissions from a filtered contact list.                                                                                                                              |
| Mailbox receive cadence | `core/node/serving.rs`, `participant_runtime.rs` | Reuse the participant-owned two-exchange receive worker, independent relay due times and the existing authorized one-office exchange. The primary slot and rotating alternative preserve healthy cadence and slow-source progress. |
| Group recovery/custody                         | `core/node/channel_custody.rs`, `channel_projection_cache.rs`, `session.rs`                                                                                                     | Reuse accepted membership and retained-event reconciliation; availability is not an unconditional delivery guarantee.                                                                                                                  |
| Durable outgoing delivery                      | `core/node/delivery_store.rs`, `delivery_runtime.rs`, `delivery_receipt.rs`, `delivery_observation.rs`                                                                          | Storage owns persisted transitions; runtime owns bounded direct/relay progress and scheduling; receipts own exact completion evidence. Runtime observations describe exact attempts under the [delivery observation contract](network-design-reference.md#runtime-delivery-observations). |
| Post-office routing/federation                 | `core/postoffice/` (including signed network membership in `network.rs`), `core/node/postbox.rs`, `opaque_relay.rs`, `bridge_directory.rs`, `bridge_client.rs`, `relay_policy.rs`                                                     | Reuse relay eligibility, opaque projection and incremental mailbox admission; directory participation does not grant relay or conversation authority.                                                                                  |
| Transport/session reuse                        | `core/transport/`, `core/node/transport.rs`, `session_pool.rs`, `session.rs`, `account_session.rs`                                                                                                    | Reuse authenticated bounded sessions; `session_pool.rs` also owns exact-endpoint authentication admission before TCP permits. Feature modules do not open ad hoc sockets or create their own retry loops.                                                                                                                      |
| Files/folders                                  | `core/file/`, `core/node/files.rs`, `sticker_media.rs`, `filebook.rs`, `file_consent.rs`, `file_completion.rs`, `file_message_identity.rs`, `group_file_delivery.rs`                                                                                      | Consent, chunk integrity and recipient completion remain distinct transitions. Presentation intake belongs to `ui/features/chat/attachments/useAttachmentIntake.ts`; the store retains intent acceptance/retry.                        |
| History/search/recall/reactions                | `core/node/queries.rs`, `search.rs`, `recall.rs`, `reactions.rs`                                                                                                                | Query through core; [recent-search references](ARCHITECTURE.md#recent-search-references) reuse canonical history identity and coverage. Frontend uses verified account bindings and centralized contact visibility.                                                                                                                                        |
| Linking and sibling replay                     | `core/node/pairing.rs`, `linking.rs`, `enrollment.rs`, `account_sync*.rs`, `account_preferences.rs`, `account_access.rs`                                                                           | Pairing authority and replay policy stay separate from the dialog. Host adoption belongs to `src-tauri/src/linked_account.rs`; signed-out enrollment belongs to `device_enrollment.rs` and reuses the recovery publication journal; private login application belongs to `shared_account_login.rs`.                                                                                            |
| Calls                                          | `core/node/call.rs`, `ui/store/calls.ts`, `ui/lib/mediaDeviceCheck.ts`, `ui/features/chat/settings/MediaDeviceCheck.tsx`                                                                                               | Core owns authenticated signaling; the call store owns call lifecycle and WebRTC media; the settings device-check runner owns local media diagnostics. Do not reuse mailbox custody as live signaling.                                                  |
| Auth/runtime ownership                         | `src-tauri/src/state.rs`, `chat_commands/state.rs`, `runtime_bootstrap.rs`, `runtime_admission.rs`, `linked_account.rs`, `owner_commands.rs`, `owner_commands/search.rs`                                    | Reuse session leases and runtime installation checks; commands adapt inputs and errors. Frontend work uses `ui/store/ownership.ts`.                                                                                                    |
| Message UI transitions                         | `ui/store/chat.ts`, `outgoingIntent.ts`, `conversationState.ts`, `chatRuntimeEffects.ts`                                                                                        | Intents own acceptance, conversation state owns cache transitions, effects bridge feature stores. Do not settle messages independently in components.                                                                                  |
| Settings/privacy                               | `src-tauri/src/settings.rs`, `privacy_commands.rs`, `ui/store/settings.ts`, `privacy.ts`                                                                                        | `ui/lib/privacySettings.ts` owns presentation defaults/projection; `settingsDialogSession.ts` owns load/save generations. Network admission remains in core.                                                                           |
| Contact hiding                                 | `src-tauri/src/contact_policy.rs`, `ui/store/contactPolicy.ts`, `ui/lib/contactVisibility.ts`                                                                                   | A presentation filter, not a communication block. Reuse it for search and pickers; preserve shared groups and incoming delivery.                                                                                                       |
| Personal conversation removal | `core/node/account_documents/personal.rs`, `core/node/lifecycle.rs`, `src-tauri/src/contact_policy/deletion.rs`, `owner_commands/group_operations.rs` | Core owns durable account-scoped intent, deletion floors and retryable local cleanup. Host adapts exact-runtime commands; UI offers history choices and clears projections only after actual cleanup. Personal removal never dissolves a group or recalls other accounts' copies. |
| Backup/recovery                                | `core/recovery/`, `core/node/recovery.rs`, `src-tauri/src/recovery.rs`                                                                                                          | Core validates archive/snapshot; host owns staging/publication and runtime barriers; UI presents inspection and confirmation.                                                                                                          |
| Appearance/avatars/packs                       | `ui/store/avatars.ts`, `packs.ts`, `ui/lib/theme.ts`, `pack.ts`, `ui/components/identity/`                                                                                      | Reuse identity rendering and pack validation. Do not create feature-specific avatar stores or variant parsers.                                                                                                                         |
| Diagnostics/updates/native controls            | `src-tauri/src/diagnostics.rs`, `updates.rs`, `window_geometry.rs`, `window_visibility.rs`, `ui/features/chat/DiagnosticsDialog.tsx`, `diagnostics/useDiagnosticsController.ts` | Keep OS operations in host. Live views reuse `useSerialPoll.ts`; diagnostic observations are not policy authority.                                                                                                                     |

These owners already exist. Adding a new abstraction is justified by a shared
business decision or a real lifecycle seam, not merely by similar syntax. For
example, discovery TTL and conversation presence need not share a constant just
because both measure time; session admission and mailbox admission must retain
their distinct authorization contracts.

### Inline media presentation

`ui/features/chat/MediaPreview.tsx` owns ordinary inline image/video presentation.
Receiving, loading and unsupported states use a responsive provisional frame
(up to 320 pixels wide, 4:3 for images and 16:9 for video). On image decode,
the frame adopts the original aspect ratio within 320 by 320 pixels without
upscaling or cropping. Images have no padded text bubble; quoted thumbnails
fit their original ratio within 64 by 48 pixels. The lightbox retains full-size
access. Decoded image geometry is cached by live Blob URL (at most 64 entries)
so virtualized remounts can reserve the known size; initial decode may resize
a row, with reader anchors and live-edge intent owned by the viewport. Transfer readiness
must not swap an ordinary preview from a compact file card to an image row.
The ordinary-image lightbox owns pointer-anchored wheel zoom (1–8 times the
window-fit size), double-click zoom, bounded pointer dragging, fit reset and
keyboard zoom (+/−/0). Resize clamps the current position; reopening resets it.
Controls remain outside the clipped image stage. Escape restores trigger focus.
On macOS, unzoomed images retain native text selection; Option/Alt-drag bypasses
panning when zoomed. Actual recognition and selection require WKWebView evidence.
Ordinary-image context actions use the shared accessible menu, suppress WebKit
browser defaults and reuse the attachment save/reveal owners. `ui/lib/imageClipboard.ts`
exports the entire decoded image as PNG, starts the write within user activation,
and guards delayed encoding with current-session authority. Image copying honors
confirmation and best-effort unchanged-content clearing. File sharing is exposed
only after a platform capability check; native subject extraction is not exposed.
Escape dismisses the action menu before dismissing the preview.
Attachment consent remains owned by `FileBubble` and the existing file policy.

`mediaFile.tsx` owns durable-first reads and explicitly opts ordinary previews
into `mediaObjectUrlCache.ts`. Concurrent consumers share one admitted read and
Blob URL. Active views hold references; idle reuse is limited to 64 entries,
32 MiB of Blob bytes, and 30 seconds since the last view released its reference.
Oversized media is released on last use instead of evicting smaller idle media.
Auth generation, owner, chat runtime, identity or readiness retirement revokes
stale URLs and clears video posters; retired reads cannot initiate fallback or
publish a URL. Protected reveals keep the default per-mount URL lifecycle and
never opt into ordinary-media reuse.

`useConversationViewport.ts` owns reader anchors and live-edge intent alongside
Virtuoso's variable-height measurements. Pending user scroll input prevents old
anchors from writing `scrollTop` during insertions or resize callbacks. Explicit
navigation to the latest message clears earlier momentum intent so delayed row
measurements keep following the live edge. Existing message keys remain the
virtual row identities. Browser scenarios cover stable
receive/decode geometry, remount reuse and owner cleanup; native trackpad momentum
and WKWebView decoding still require platform evidence.

### Coordinated state owners

`core/eventlog/repository/` owns physical event counts and record-cost estimates,
including their append and removal updates, conversation retirement authority,
and bounded graph reclamation. Its admission gate preserves validation and
counter errors before reserving new growth or allocating a stored-event encoding.
`core/postoffice/storage/` owns relay first-arrival time, touch, role readiness,
monotonic expiry visibility, capacity backpressure and persistent drain work on
the same profile connection. Relay transactions reuse repository admission and
physical charge rather than maintaining parallel counters. Indexed role pending
state changes with repository retirement, and matching arrival removal shares
its finalization transaction. Policy-bearing reads qualify current eligibility;
physical diagnostics alone cannot qualify custody. Runtime and paced worker
adoption remain separate. See [storage ownership and budgets](ARCHITECTURE.md#storage-replay-and-query-budgets).

`core/storage/profile/participant.rs` owns participant control metadata, explicit
source locators and recoverable account-adoption intent on the shared connection.
Its `participant/event_ownership.rs` owner commits account-qualified event ordinal
ranges and a monotonic captured-owner generation with adoption; receive schedulers
derive indexed namespace selectors from those exact ranges rather than inferring
ownership from conversation IDs or replacing an older account's pending work.
`core/node/delivery_runtime.rs` remains the account-adoption decision owner and
coordinates its existing delivery journal with that intent.
`core/storage/profile/participant_events.rs` owns independent event-source
publication, its downgrade barrier, authenticated import and retryable cleanup.
`core/eventlog/persist.rs` selects the profile-backed repository for production
Nodes; `persist/legacy.rs` retains the explicit standalone compatibility owner.
`core/node/privacy_sync/network.rs` applies participant authorization to finite
repository network pages. `core/eventlog/repository/selection.rs` owns disposable
SQL authorization IDs, causal closure, and bounded retirement cleanup.
`core/node/privacy_sync/principal/` and `principal.rs` own current principal state
and resumable preparation; historical IDs, departures and ancestry stay on disk.
`core/node/session.rs` releases the store lock and yields between preparation
turns. Ready views use scalar source/proof checks. Channel-control authority still
uses the checkpoint owner; its individual ancestry queries remain a separate
bounded-work integration requirement.
`core/node/channel/checkpoint/` owns indexed current membership, canonical authority,
receiver checkpoints, exact processing evidence, activity pages and pending retries.
Ordinary warm channel opens load current metadata and the active sending epoch;
completed signed history is not replayed. Other participant domains retain their
legacy owners until their own cutovers. `core/node/runtime_startup.rs` composes
startup inputs and completion through `runtime_work.rs`; blocking captures and
undelivered results remain admitted until their resources are destroyed.
The desktop startup permit retains replacement serialization, and backup consumes
the retired owner only after matching its host local-user label and physical
profile root. Core does not derive desktop authority from account keys or paths.

`core/node/transcript_repository/` owns indexed transcript headers, bodies,
metadata counters and independent source publication on the shared profile
connection. `account_file_repository.rs` and `file_message_identity/indexed.rs`
reuse that publication owner for signed file proofs and exact logical-file
bindings, with fixed headers, bounded owned payload reads and persisted revisions.
`filebook/indexed.rs` owns validated file eligibility metadata and
exact processed facts tied to existing FILES ordinals; its account namespace,
resumable import and persisted revision use the same connection. Production Node
uses this owner directly. `filebook/scopes.rs` owns source 9 sent fanout references;
`filebook/work.rs` owns metadata-only download discovery, fair bounded admission and
revision-qualified completion. `filebook/receiving.rs` owns metadata-only manifest
discovery and fair retry admission, retaining oversized rows until an adequate
budget is supplied and using exact file processing evidence for completion.
Discovery qualifies each commit through the participant-owned account intervals
before inserting pending work; selection applies its account index before LIMIT.
Captured adoption generations fence old handles even after returning to an account.
`attachment_consent/indexed.rs` owns account-local receive preferences, exact offer
decisions and their durable ordering/generation under source 17. Its selected
policy reads feed the existing file eligibility and download owners; signed
remote controls remain the separate `file_consent_store.rs` contract.
These modules own neither a second payload journal nor a permission
rule; network disclosure reuses the existing exact parent authorization.
`sent_store.rs`, `received_store.rs` and the indexed owner in
`account_suppression.rs` expose owned lookups, bounded pages and exact local
deletion markers. Their constructors require validated participant authority;
production Node history, recovery, recall, and account-sync consumers use these
indexed stores. Media bytes retain their independent encryption/lifetime owner. `eventlog/repository/order/combined.rs` extends the existing causal
order owner with disk ranks for signed events and isolated archive roots.
`eventlog/repository/receiving.rs` owns resumable metadata discovery and bounded,
fair retry selection; exact transcript evidence survives body erasure and commits
with body/effect installation. These derived selectors do not replace the delivery
journal. Account-route selectors retain the first envelope per conversation and
advance transactionally when its row is erased.
See [storage ownership and budgets](ARCHITECTURE.md#storage-replay-and-query-budgets).

`core/node/opaque_receive_cache.rs` owns optional disk-backed mailbox hints on
that same profile connection. Its independent source publication streams legacy
mappings once; warm open reads metadata only. Active mailbox sync rechecks exact
inner-event retention before using durable hints, while session-only progress is
fenced by a fresh run identifier. ID sets exist only for the bounded active wire
exchange. Corrupt or foreign hint sources remain preserved and unavailable; hints
never establish admission, delivery, or deletion authority.

`core/node/reaction_store/indexed.rs` owns the exact locally authored reaction
projection. Its independently published source is keyed by the full account
public key, preserving the legacy per-account path across adoption. The common
publication state machine keeps unscoped history identities unchanged; only its
registry selector varies. Node warm open reads no authored reaction payloads;
conversation and target pages admit keys and bytes before bodies. Unsigned action
clocks and remove-wins ties are unchanged. Bulk physical cleanup uses bounded
transactions and evaluates external predicates outside the SQL mutex. Incoming
reaction aggregation and the account-document startup owner retain separate
consumer work; this source cut does not establish bounded reaction-view queries.

`core/node/reactions/selected.rs` owns selected-target reaction preparation and
summary pages. Its SQL projection reduces the latest authenticated action per
principal into counts and a local-author flag; it does not materialize author
lists. Requests bind the account generation, selected targets, proofs, channel
control generation, deletion visibility and relevant source revisions. A pending
response is empty and includes its continuation. Key/byte admission precedes
retained-body reads and summary-string allocation; unchanged completed queries
reuse the persisted result, and ordinary message tails preserve the reaction
source token. Modern-channel causal preparation still depends on the shared
checkpoint's resumable query integration. Legacy collecting view APIs remain for
the pending native/frontend cutover, so this query prefix alone does not establish
an end-to-end bounded reaction UI or change the account-document startup owner.

Protected text and image messages extend `core/node/message.rs`, `file/manifest.rs`,
`files.rs`, `queries.rs` and `search.rs`: the authenticated body owns its protection mode, while ordinary
projections omit the content. The explicit exact-message content lookup is for
an authorized host reveal, not a second history/search implementation.
`src-tauri/src/owner_commands/search.rs` owns that reveal's session/runtime and
reference checks and reuses the credential verifier in `app_lock.rs` without
unlocking the application. The frontend message view owns the temporary
30-second reveal and temporary image URLs; revealed bodies must not enter
conversation caches, reply
quotes or notification previews. The [privacy guide](privacy-controls.md#protected-messages)
owns the product behavior and limitations.

Sensitive settings, saved-login retry, agent grants, backup publication and
device linking reuse the per-operation proof in `src-tauri/src/app_lock.rs`.
`runtime_admission.rs` carries its exact session and lock revision through native
commit boundaries; it is never a renderer-issued or reusable unlock token.
`settings.rs` owns effective downgrade decisions and the local protection floor
for imported preferences. Generic preference IPC cannot write its `settings.*`
namespace. `ui/lib/api/sensitiveInvoke.ts` retries only the captured operation
when the backend requests a password; the app-owned
`ui/features/auth/SensitiveVerificationDialog.tsx` supplies the prompt and cancels
it on session retirement. The [sensitive-operation contract](privacy-controls.md#verify-sensitive-operations)
owns user behavior and platform limits.

`ui/store/conversationHistory.ts` owns active history reloads, bounded background
coverage scans and expansion to a requested Event. It shares history selection
and coverage/reception commits across those paths. The roster supplies discovered
Conversations and import revisions; `outgoingIntent.ts` retains acceptance and
retry. `reception.ts` owns identity-based receipt/unread decisions, including the
legacy unread projection. Channel receipts retain a content-free mention flag,
classified with the same display-name matcher as message rendering. The sidebar
shows a localized mention prefix alongside the latest preview and unread count
while any retained mention identity is unseen. Opening a conversation or receiving
ordinary messages does not acknowledge mentions; exact viewport read evidence
does. Owner/account-scoped receipt persistence preserves the flag independently
of message-cache eviction. Own-account and protected messages never classify as
mention alerts. `conversationState.ts` retains cache, deletion and
optimistic-message reconciliation. History callers do not coordinate those
internal transitions or import back into the chat composition module at runtime.
DM file status refresh continues after the file offer's receipt, until the
separate file-progress projection reaches completion. Receipt of the offer must
not freeze an earlier approval or byte-progress snapshot on the sender.
Active history and reaction reads use the shared native-read deadline so a stalled
IPC request releases the loading indicator into the existing retry state. Late
results from a timed-out read cannot replace a newer snapshot. The chat identity
boot window is thirty seconds of elapsed monotonic time, including native waits;
expiry exposes failure and the existing retry action. While failed, the same
owner/run ticker performs one bounded identity recovery attempt at most every ten
seconds, preserving the failure label until identity resolves. Successful recovery
clears failure and reloads favorites and roster without a restart. Logout or a
superseding run invalidates delayed results and stops its ticker. These presentation
deadlines do not cancel native work or establish whether the participant runtime is healthy.
The history module defines its own state/context interface; `chat.ts` composes it
and supplies runtime generations plus status refresh. Its patches cannot mutate
that session context. `conversationModel.ts` owns shared Conversation/message data
types, with established chat exports preserved. History and cache initialization
are defined by their respective owners and reused by production session resets
and standalone history tests, which do not initialize the aggregate chat store.
Outgoing intents likewise define their own state/context contract, composing the
shared cache state with runtime validity, sender identity, reload, status refresh
and error-reporting capabilities. Their patches cannot mutate unrelated chat
state. Chat supplies these capabilities; standalone send/status tests use the
same cache defaults without initializing chat. Feature work should extend these
owners instead of importing the aggregate chat getter/setter into them.

The text composer transfers submitted content synchronously to the existing bounded,
owner-scoped outgoing intent. A refused local submission retains the draft and
creates no bubble. An admitted submission clears the input immediately; its bubble
alone owns pending acceptance, failure and retry. Backend acceptance and recipient
confirmation remain distinct states, and later completion cannot overwrite a new
draft or put submitted text back into the composer.

`ui/store/drafts.ts` owns reactive conversation text, protection choices, revision-bound
admission and one idle-save coordinator
per local user/session; both composer restoration and inactive sidebar previews
consume it. Typing updates memory without re-rendering the conversation view.
Persistence coalesces after ten seconds idle, restarting the timer on each edit;
navigation and visibility loss request a flush. Session retirement discards
unsaved memory, and abrupt exits may lose recent edits. `src-tauri/src/drafts.rs`
adapts typed account-document patches under the exact desktop/runtime account.
The core account-document store owns encrypted metadata, signed revisions, deletion
tombstones, content-addressed blobs and bounded existing-session reconciliation.
Text and protection metadata share one version. Whole-view absence never deletes
another device's draft. Legacy text-only maps remain read-only until explicit kept-
source preparation can attribute known conversations; ambiguous IDs stay untouched.
Pending
admission and callbacks stay in memory. Success clears only its originating draft
revision and flushes that removal; rejection retains the draft as the sole retry
owner. Protected inactive previews and tooltips conceal draft text.
See [draft behavior](help/topics/history.md#keep-an-unfinished-message).

Protected text uses durable acceptance before clearing its captured draft, reply
and protection choice. If that attempt is rejected, its temporary optimistic
intent is removed and the composer remains the sole retry owner. Protected images
transfer their source to the outgoing intent, whose failed bubble alone owns retry.
Channel text, stickers and file offers use owner-scoped acceptance IPC returning
an exact durable event ID (and file-conversation ID for attachments). Network
fanout retains its own runtime admission and cannot hold the acceptance response
open. History merges the accepted ID into the same optimistic bubble; it never
matches equal content or leaves a second identity-free pending bubble behind.
Member-copy and post-office-copy status refresh independently of acceptance.
Nickname publication uses a shared runtime lease and the runtime's small metadata
lock. It must not enqueue a lifecycle writer behind admitted network work, because
writer preference would then block identity, presence and history observations.
Exclusive runtime admission is reserved for installation, retirement and operations
that actually replace runtime-owned resources.

Account DM arrivals carry the authenticated sender account and the envelope's
logical message ID from core through desktop IPC. The receipt ledger uses that
same identity as account history, so live callbacks, repeat delivery and history
coverage cannot count one logical message twice. File messages use the sender's
explicit signed binding of device manifests to a logical file message; their
exact consent/completion IDs remain separate. Core history exposes authenticated
aliases to the host. The reception ledger merges late alias evidence under that
canonical identity, preserves read facts and bounds optional alias metadata
independently of unread entries. Independent messages with equal
text retain separate IDs and counts. Legacy providers without a logical identity
retain their compatibility counter path.

`ui/store/accountGroupOperations.ts` coordinates typed group operation IPC through
explicit lease and state ports. It retains logical operation IDs through
uncertain replies and separates pending, committed, rejected and cancellation
outcomes. `outgoingIntent/groupOperations.ts` binds body-free tickets to the
existing aggregate outgoing state with atomic capacity and protected retention;
it does not create another message cache. Original native invocation lifetime
protects terminal tickets from eviction, as do message-intent references.
The existing ticker and action/message owners must supply observation scheduling,
terminal presentation and exact public-event reconciliation. Session checks also
cover synchronous retirement caused by reactive state and diagnostic callbacks.

`ui/store/channelActions.ts` owns channel creation, member edits, ownership
transfer, invitation acceptance, leaving, dissolution and renaming. Its narrow
state/context defines channel projections and explicit capture, command,
navigation, refresh and error capabilities. `chat.ts` supplies these capabilities
and preserves its existing public actions. The common member-mutation sequence
is private to the owner; distinct leave/dissolve/acceptance transitions remain
explicit. Standalone tests exercise the owner without initializing chat.

Attachment approval ordering and stale-session checks belong to
`ui/features/chat/attachments/attachmentDecision.ts`, reused by message bubbles
and the files tray. Those views retain their translated errors and local controls;
core remains authoritative for consent and byte readiness. `HighlightedText.tsx`
owns shared search/history highlighting. `StickerThumb.tsx` owns decoded static
frames and bitmap cleanup, while `ui/lib/motion.ts` owns the live reduced-motion
preference. Unknown preferences disable animation; mounted consumers react to
operating-system preference changes. None of these modules owns global chat state.

`core/node/account_profile.rs` owns signed Account profile state: startup
reconstruction, local revision allocation, authenticated incoming updates and
coordinated avatar persistence. Its internal state owns the journal and current
publication together. Local avatar edits must advance the durable revision;
revision exhaustion fails before changing publication or emitting an update.
Node profile paths retain authenticated Event processing,
authorization and delivery. Recovery uses explicit snapshot/import operations;
retired language metadata is never republished or restored as filtering state.
The existing signed payload and encrypted record formats remain the same.
`core/node/account_profile_data.rs` and `account_profile_io.rs` own the
canonical account nickname, biography, avatar and immutable creation metadata.
The `account_profile_data/indexed.rs`, `profile_store/indexed.rs` and
`trust_card/indexed.rs` owners keep their distinct current-value admission rules
and store payloads on the shared encrypted participant connection. Each source
uses the independent history publisher and legacy-writer barrier. Warm owner
construction reads metadata only; current pages admit at most 16 keys and 1 MiB
before loading bodies, and selected lookups propagate storage errors. Signed
account profiles preserve clear-avatar tie ordering and immutable creation dates;
frozen trust cards keep the first accepted equal revision. Existing signed-owner
account and wire-byte quotas remain unchanged. Node authenticates cold sources before publication and opens these indexes after
participant authority completes. Accepted signed values atomically mark their
current row for avatar projection; an exact change-revision acknowledgement clears
that marker only after projection succeeds. Startup resumes bounded pending keys,
so an unrelated successful write cannot hide failed work and unchanged warm opens
do not scan peer payloads. Own publication retains only the current local avatar.
Nickname-only edits preserve other profile fields and use the same revision ordering. `src-tauri/src/runtime_bootstrap.rs` projects accepted own
account nicknames into local credentials, live presence and frontend state under
exact session/installation authority. Its runtime-owned producer retires with the
node and joins admitted blocking persistence. A host-local `profile-binding.json`
account marker migrates a legacy singleton source name once. An old string-only
marker that already names the current account but has no authenticated profile repairs that
missing shared profile from the established local nickname. The versioned object
marker also records an adopted account awaiting its source profile, so repeated
startup cannot publish the joining device's unrelated previous nickname. Existing
authenticated profiles always win over legacy-marker repair. Contact nickname selection
remains in `core/node/runtime.rs` and the durable name directory: verified
announcement nicknames outrank stale legacy profile labels. This does not change
the authenticated profile fields themselves or create personal aliases.

`core/identity/account_login.rs` owns the versioned per-local-user credential
envelope, canonical login resolution and the shared login-name namespace lock
used by registration and migration. Its authenticated payload repeats the public
account/local-user binding. Atomic publication preserves the storage username,
local-user UUID and separate identity/node storage passwords; migration never
rewrites retained history. A migrated user cannot fall back to the old login, and
corrupt envelopes fail closed. Collisions preserve existing users. Guarded
publication rechecks host authority after encryption; explicit account adoption
also checks the previous bound account.

`core/node/account_access.rs` owns bounded, signed, account-scoped opaque access
records, their encrypted journal, revision admission and certified-sibling
exchange. This exchange is independent of optional sync categories. Credentials
never enter public profiles, discovery or ordinary message events.
`src-tauri/src/shared_account_login.rs` owns the private desktop payload and its
explicit source initialization, pre-adoption name check, device/account-bound
adoption intent and session-guarded envelope installation. Its runtime-owned
reconciliation updates the canonical session login and saved-login state only for
the admitted owner. Target startup never seeds unrelated previous credentials.

`src-tauri/src/desktop_session.rs` owns the active desktop user, token, retained
identity/node storage passwords, shared account login password and exact-session
generation. Credential authentication and profile persistence
delegate active-session transitions to that shared owner. Commands retain auth
operation lifetime, runtime installation and retirement, without maintaining a
second active user/token representation. `state.rs` preserves established exports;
`runtime_admission.rs` continues to own runtime/session commit authority.

Core runtime preparation exposes only verified public device/account IDs after
its existing keystore unlocks. The desktop runtime state owns the exact-session,
startup-ticket observation and its invalidation; it grants no early runtime
admission. The chat store binds that observation to its current owner/run and
hydrates persisted reception state once before full readiness. Appearance then
uses the existing account-scoped pack cache, prioritizing the selected theme.
Transient readiness changes do not retire an unchanged appearance scope; session
or account replacement clears both the private cache projection and its CSS.

`frontend/src/App.tsx` owns this authenticated lifetime before lazy chat loading.
`lib/initialPresentation.ts` prepares its authorized image and committed layout;
it does not choose another account or restore a second theme projection.
`src-tauri/src/window_visibility.rs` owns one-shot initial presentation, explicit
visibility precedence and its bounded fallback. Window-state persistence retains
geometry and decoration, while launch policy owns visibility. A renderer
acknowledgement is presentation-only and grants no runtime or content authority.
Explicit main-window restoration also belongs to this owner: Dock reopen,
single-instance activation and tray actions reuse its show/unminimize/focus path.

### Focused validation ownership

Module-owned `validation.json` contracts link these boundaries to development
checks. Account profiles use `core-account-profile`; desktop session/admission
uses `desktop-session`; history, outgoing intents, shared conversation state and
chat composition use `frontend-history`, `frontend-outgoing`,
`frontend-conversation-state` and `frontend-chat`. Channel actions use
`frontend-channel-actions`, with chat as a declared consumer. Contracts declare downstream
consumer IDs and escalation notes; frontend source imports add affected unit
consumers automatically. Integration grant policy expands through service,
headless host, IPC, client and desktop/MCP adapters. Rust source, package and
shared-fixture inputs have automatic owners with conservative downstream
expansion. Shared Cargo/toolchain inputs select every workspace crate. Binary
and integration-harness owners may use library fallback selectors with explicit
escalation; those selectors do not establish native, CLI or integration
acceptance. Unknown paths still require ownership or explicit reviewed selection.

Shared `SendIntent` and `SendPayload` data shapes belong to `conversationModel.ts`.
Conversation-cache policy depends on this model rather than outgoing dispatch;
the dispatcher preserves its public type reexports. Focused internal store
consumers use existing IPC adapters such as `api/chat.ts` and `api/contacts.ts`.
The public facade remains available at the application composition boundary.

A module change should ship its focused behavior evidence and update its contract
when it changes consumer obligations or shared fixtures. Interface changes require
consumer validation; runtime/native behavior can require browser or process
checks. See [change-aware development plans](testing-feedback-loop.md#change-aware-development-plans)
for preview/run commands, unknown-path handling and the unchanged mandatory gates.

### Desktop update channels

`src-tauri/src/updates/channel.rs` owns channel eligibility and immutable build ordering;
`updates.rs` owns bounded GitHub discovery, exact-release revalidation and verified manual downloads.
`updates/native.rs` owns signed native payload selection, private staging, version-bound verification
and platform installer/restart handoff. The host never accepts installer paths or trust keys from IPC.
`ui/store/updateRestart.ts` coordinates draft persistence and active-work deferral before installation.
The frontend update store owns the device-local channel preference and invalidation of checked
candidates; its IPC adapter forwards the choice without choosing packages. The existing release
workflow and `scripts/release/` own build identity, publication and provenance.
Root Cargo `workspace.package.version` owns the next numeric product target;
`scripts/release/version.mjs` owns version preparation, checked projections and drift detection.
Tauri inherits Cargo, root private npm metadata is a generated release-tooling projection,
and the private frontend metadata is versionless. `scripts/release/maintenance.mjs` and
`prepare-stable.yml` own explicit, main-only draft/tag preparation and build dispatch;
`release-maintenance.yml` owns only reviewed post-Stable next-target PRs. `CHANGELOG.md` owns
curated product notes and the release renderer combines the exact version section with shared
installation/verification instructions. These owners preserve the final verified Stable publication gate.
Source/Nightly/Stable logical identities follow SemVer independently of numeric native package
metadata. New consumers extend these owners rather than creating manual version copies. See
[updates and publication](updates.md) for invariants, triggers and native acceptance requirements.
Channel choice does not change protocol, account/device identity, data location or package naming.

## 3. Sidebar responsibilities

`ui/features/chat/Sidebar.tsx` is the composition root: it selects application
state and visibility, coordinates rename/restore actions, and arranges the
sidebar shell. Independent work belongs in `ui/features/chat/sidebar/`:

| Module                         | Responsibility                                                | Interface contract                                                                                                                                                           |
| ------------------------------ | ------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `conversationList.ts`          | Pure row projection and account-level people counts           | Stable source order; account pins before channel pins; aliases affect display but retain original rename names; hidden groups remain restorable and leave navigation totals. |
| `ConversationSection.tsx`      | Render a section with ordinary rows or Virtuoso               | Same row/action path for both modes; virtualize above 80 rows; preserve 56px estimated height, section position offsets and common scroll parent.                            |
| `ConversationRow.tsx`          | One conversation's preview, unread state, profile and actions | Reads isolated conversation/presence state; inactive channel status takes precedence over stale message previews.                                                            |
| `useConversationNavigation.ts` | Keyboard focus and virtual scroll coordination                | Arrows apply only to conversation option buttons; scrolling uses section-local indices; delayed row mounting must not steal focus from another control.                      |
| `useSidebarWidth.ts`           | Resize/persist lifecycle                                      | Existing storage key, 284px default, 230–460px bounds, pointer cleanup, keyboard resizing and reset. Storage failure must not prevent rendering/resizing.                    |
| `SidebarUtilities.tsx`         | Secondary menu and its dialogs                                | Own menu/dialog state, calls feature gate, theme action, lock and logout.                                                                                                    |

There is one list renderer, rather than separate pinned/contact/channel copies
for short and virtualized lists. People counting shares one pass over the roster,
deduplicates accounts, excludes self and relays, and retains the intentional
distinction between network availability and hidden-contact presentation. Public
contact rows remain withheld until the visibility policy has loaded.

Periodic network observations use `useSerialPoll.ts`, shared with other live
views: no overlapping requests in one subscription and no results applied after
that subscription closes. Store-level owner/session checks remain mandatory;
serial polling is not an authorization mechanism.

Optional registration-origin disclosure stays with the existing network owners:
`core/node/discovery_origin.rs` owns bounded consent and office signatures;
`bridge_directory.rs` and `postoffice/federation.rs` own negotiated carriage and
fixed expiry; `bridge_client.rs` owns source-side publication permission.
Roster observations retain the validated proof without creating routing authority.
Desktop presence IPC projects only office identity, name and remaining lifetime;
the frontend ages this lifetime independently during failed refreshes. The existing
account settings owner persists the default-off device choice. See
[registration-origin disclosure](network-design-reference.md#optional-registration-origin-disclosure).

## 4. Parallel development contract

1. **Find the owner first.** Consult the table, inspect its public interface and
   current callers, and extend the existing rule before adding another helper.
2. **Assign work by responsibility.** For example, conversation preview work owns
   `ConversationRow`; ordering work owns `conversationList`; resize behavior owns
   `useSidebarWidth`. These changes should not all edit `Sidebar.tsx`.
3. **Agree on shared shapes early.** Changes to IPC types, the Node public facade,
   wire formats, runtime ownership, or store interfaces require one integration
   owner. Parallel agents can implement callers against the agreed interface.
4. **Keep composition changes small.** Wire a new module at the feature root;
   avoid moving unrelated imports, reformatting unrelated modules or editing a
   global barrel to expose feature-private implementation details.
5. **Retain feature locality.** A feature-specific helper stays in its feature
   folder. Promote a helper to `lib/` only when actual callers share its meaning.
6. **Preserve evidence and compatibility.** Do not remove old entry points or
   change error/serialization contracts while extracting an internal module.
7. **Test at the owning interface.** Pure projections use table/scenario tests;
   hooks use mounted lifecycle tests; protocol changes need the relevant core
   integration assertion. Avoid source-text tests that punish harmless moves.

Before parallel implementation, record each task's owned paths, the existing
decision owner it will reuse, any required shared-interface change, and its
focused verification command. Include behavioral contracts (authority, ordering,
cancellation, errors and persistence) when agreeing on an interface. A shared
helper should own the decision and its invariants so callers do not each repeat
the same validation or transition sequence.

One integration owner coordinates edits to shared types, command registrations,
module declarations, dependency manifests and composition roots. Agents can
propose these edits in their handoff while implementing their own modules.
Separate worktrees isolate edits but do not resolve overlapping responsibilities.
Avoid broad formatting, unrelated moves and feature-private global exports.

Each handoff lists changed paths, interface changes, reused owners, executed
checks and any assumptions that affect other tasks. After integrating, rerun the
owner's focused tests and affected callers' checks; a clean Git merge alone does
not establish behavioral compatibility. Update this ownership map when a decision
moves, rather than maintaining a second competing map.

Module division reduces overlapping edits; it cannot guarantee conflict-free
parallel work. Cross-feature policy changes still need coordination and review.

## 5. Focused verification

For the sidebar/settings seams:

```sh
cd frontend
npm test -- src/features/chat/Sidebar.test.ts src/features/chat/sidebar src/features/chat/useSerialPoll.test.ts src/store/settings.test.ts src/features/chat/settingsDialogSession.test.ts src/lib/rosterProjection.test.ts src/lib/moduleBoundaries.test.ts
npm run typecheck
npx eslint src/features/chat/Sidebar.tsx src/features/chat/sidebar src/features/chat/SettingsDialog.tsx src/store/settings.ts src/lib/privacySettings.ts src/lib/api
```

Run a production frontend build after moving import seams to check bundling and
browser-target compatibility. Use targeted Playwright cases when interaction or
layout changes require browser evidence. Full-suite and physical multi-machine
runs remain CI/pre-PR/release or explicit-request gates, as described in the
[testing feedback loop](testing-feedback-loop.md).

### Local call-device checks

`ui/lib/mediaDeviceCheck.ts` owns independent capture sessions, abort/deadline
handling and same-WebView receive-statistics diagnostics. The settings panel
`ui/features/chat/settings/MediaDeviceCheck.tsx` owns explicit start/stop, muted
preview, microphone analysis and runtime/view retirement. Checks do not send call
signaling or grant network authority. The call store remains the owner of actual
call negotiation and termination. See [device checks](settings.md#microphone-and-camera-check).

## 6. Remaining opportunities and limits

- Settings categories now live in `features/chat/settings/`, composed by
  `SettingsDialog.tsx`. `useSettingsController.ts` shares acknowledged values and
  delegates owner/view validity to `settingsDialogSession.ts`; `model.ts` owns
  navigation groups, category order, control destinations, dependency reasons and
  retention tightening. Language belongs to Appearance & language; screenshot
  recording and registration belong to Shortcuts. See [settings](settings.md).
  Future settings must extend the relevant category and shared rules rather than
  adding another independent save coordinator.
- `CameraCaptureDialog.tsx` owns camera stream acquisition, still capture and preview
  cleanup. `useAttachmentIntake.ts` owns its conversation/runtime lease and stages
  confirmed photos through the existing composer queue; only Send submits them.
  Late camera permission and encoding results cannot outlive their dialog. See
  [taking a photo](help/topics/send-files.md#take-a-photo).
- `ConversationView.tsx` delegates picker, drop, paste and screenshot intake to
  `attachments/useAttachmentIntake.ts`. Acquisition owns conversation/runtime
  leases, staging cleanup and source-specific media intent; store intents retain
  accepted paths and retry ownership. Screenshot edits retain their originating
  conversation rather than targeting whichever conversation is active later.
  `ScreenshotEditor.tsx` copies the final cropped/annotated PNG to the clipboard
  on confirmation before staging the same bytes; copy failures retain the editor
  for retry and cancellation never writes the clipboard. Confirmed screenshots
  join the composer's pending files; only the normal Send action hands them to delivery. `screenshot/` owns image-coordinate geometry,
  annotation painting, floating controls and temporary fullscreen restoration.
  Its shortcut router shares that intake path for local and opt-in global
  activation. `src-tauri/src/screenshot_shortcuts.rs` owns OS registration and
  native session/lock authority; the settings controller owns acknowledged local
  bindings. Registration tokens prevent retired renderer cleanup from releasing
  newer keys. See [screenshot shortcuts](settings.md#screenshot-shortcuts).
- `DiagnosticsDialog.tsx` delegates observations and native actions to
  `diagnostics/useDiagnosticsController.ts`. Each opening owns independent
  refresh generations, serial peer polling, bounded activity history and
  transient feedback. Delayed paths cannot open/save/copy after that opening
  closes. Native effects already initiated cannot be undone.
- `runtime_admission.rs` owns explicit-owner and current-session admission for
  owner delivery commands, legacy account text/sticker/file sends, privacy and
  linked-device synchronization/preferences. Capture
  before waiting for the runtime; synchronous operations retain matching session
  authority through the operation. `NodeState::with_session_runtime` owns the
  shared session/installation decision. A raw node handle remains a snapshot,
  not authority for future mutations or plaintext release.
  Installed-runtime operations hold shared lifecycle leases, allowing network
  producers, observations and unrelated sends to proceed concurrently. Runtime
  installation, replacement and removal retain exclusive access and wait for
  admitted operations. The matching desktop-session guard still encloses each
  synchronous commit and plaintext observation; shared access does not authorize
  stale work or allow an old runtime to survive replacement.
  Registered channel, device, file, reaction, recall and call mutations use the
  same entry lease and guarded core commit. Native file writers keep lifecycle
  ownership through cancellation. Search and file/media queries use the same
  blocking lifecycle and recheck the entry lease before releasing plaintext.
  Trust verification encloses its durable write and live recovery-policy update
  in that authority. Contact mutations acquire runtime, then policy,
  then the matching entry session; owner strings alone do not authorize them.
  Legacy payloads without an owner cannot identify a frontend intent created
  before IPC execution. A post-result check cannot prevent prior side effects.
- Live settings application is serialized and carries the originating lease plus
  a settings revision. Policy commits and blocking retention pruning check the
  latest revision while holding its guard; periodic pruning uses the same path.
  A superseded document cannot apply policy or erase history after a newer save.
  Startup follow-up retains its originating authority and uses the same guarded
  settings application; matching an account name alone is insufficient.
- `ui/lib/attachmentSave.ts` owns configured destinations, save dialogs,
  post-dialog owner/readiness checks and remembered paths for bubbles, previews,
  history, the files tray and store saves. Native writes already admitted may
  finish; stale continuations do not record paths into a replacement session.
- The call store owns local terminal signaling before resource disposal and an
  independent incoming-ring deadline. Disposing local media alone is not a
  distributed call termination. The local WebRTC diagnostic separately owns a
  run generation; closing or unmounting disposes its peers, timers and tracks,
  stops late captures and settles its pending media waits.
- Own DM/account reaction state uses an account-scoped encrypted projection with
  latest-action compaction and local erase integration. Retained peer reactions
  resolve verified historical identities rather than live presence. Reactions
  retain best-effort live fanout; certified sibling catch-up also uses the private
  account control owner below. Portable recovery snapshots do not include them.
- Causal traversal lives in `core/eventlog/order.rs` and is reused by node history
  persistence and sync exports before filtering/pagination. Signed logical clocks
  alone do not prove ancestry. Compaction checks retained ancestry before replacing records;
  authenticated replay can recover earlier noncausal file ordering. Local clock
  and author-sequence allocation return errors on exhaustion. Public `prepare`
  now returns `Result`; SDK callers must handle it despite unchanged wire formats.
- Archived channel summaries and admission use the indexed channel checkpoint's
  current metadata and its signed control reducer. Key distribution/decryption remains separate;
  archives share membership epochs, leaves, acceptance, ownership, rename and
  terminal dissolution rules with the metadata projection.
- Peer-avatar journals reclaim obsolete versions using encrypted record-log
  rewrites. They compact on opening older redundant logs and amortize subsequent
  rewrites by profile-version count. Latest clear tombstones survive compaction;
  the budget is relative to live profiles, rather than a cap on distinct accounts.
- Auth registration builds the complete encrypted user aggregate in an unpublished
  same-volume staging directory, then publishes it by directory rename. Existing
  partial directories are preserved for recovery. Subsequent encrypted metadata
  writes replace a synchronized sibling file instead of truncating the live file.
- Privacy commands reuse the same admission for checked observations and
  operation-enclosing IO commits. Core `*_guarded` methods await the privacy gate
  without host session authority, then enclose synchronous policy/proof/route
  writes, generation updates and bridge invalidation in the supplied guard.
  Public announcement drain/restore and best-effort presence publication stay
  outside synchronous authority. Existing `*_if` APIs retain their legacy
  validation/authorization ordering through the shared implementation.
- CLI and `NodeRuntime` share `PreparedParticipant`/`ParticipantRuntime` for
  participant network startup and retirement. Preparation starts no producers;
  desktop launch remains inside session authorization. Hosts adopt extra tasks;
  CLI blocking file saves hold admitted work permits even if their waiter is
  canceled. Explicit stop closes admission, aborts/joins tasks and drains blocking
  work. Drop only requests cancellation. External Node handles still require
  host lifetime discipline; a raw handle is not session authority.
- Large core delivery/channel files warrant transaction and authority review
  before further splitting. Their size alone does not show a broken interface;
  moving internals without characterization could obscure atomicity and locks.
- Large test files should be divided by behavior with existing shared test
  support. Identical-looking tests may cover different crash, authorization or
  wire-compatibility guarantees and must not be deduplicated mechanically.

The structural scan for this refactor covered production frontend import edges,
the Rust module/public-export structure, host adapters, and the feature owners
above. It is not an exhaustive line-by-line audit or proof that all bugs have
been eliminated. This file is the maintained ownership guide; Git history retains
superseded audit findings and implementation reports.

## Message forwarding

Forwarding source authorization, canonical ordering, accepted request identity and
attachment restaging belong to the protocol core. Group forwarding reuses the
account-group operation journal for admission and pending/committed lifecycle;
forwarding retains batch progress and derives stable item identities without a
second send journal. Read-only progress queries use retained operation status,
while the existing forwarding writer reconciles terminal receipts before retirement.
The desktop host supplies exact session/runtime admission. Chat features own selection, destination confirmation
and read-only record presentation; shared outgoing owners retain send acceptance
and delivery meaning. Favorites keeps its independent saved-content lifecycle.
[Message forwarding](message-forwarding.md) owns user behavior and invariants.

## Message Favorites

`core/node/favorites.rs` owns the independent encrypted collection, canonical
snapshot capture, editable metadata and retained-file authorization.
Its recovery adapter owns logical record encoding, account identity validation
and expiry projection. The existing recovery owner streams these records and
independent retained files through the same local/Drive archive and staged install.
`core/node/account_sync/favorites.rs` owns optional certified-sibling exchange.
Desktop `src-tauri/src/message_favorites.rs` owns admitted IPC adaptation;
`ui/store/messageFavorites.ts` owns query lifetimes, and
`ui/features/chat/favorites/` owns collection/selection/editing. These do not reuse
contact pin/alias preferences or Message Yourself as a collection store.
[Favorites](favorites.md) owns the limits, retention and synchronization contract.

## Shared group photos and announcements

`core/node/group_metadata.rs` owns independently versioned owner-published group
photos and announcements, encrypted publication keys, current-state releases and
durable clear tombstones. It reuses channel authority and causal ownership
validation; private account documents never grant authority over shared group
content. Group sync capability gating and event-log retention preserve this
contract across replay, offline recovery and accepted-member bootstrap.

`src-tauri/src/chat_commands/channel_metadata.rs` owns exact owner/account/runtime
admission for reads and edits. Channel summaries project validated current content
and a bounded recent announcement-card window. `ui/components/GroupAvatar.tsx`
resolves the existing private override before shared group content and the member
montage; `AvatarEditMenu` only changes personal overrides. The group editor in
`ui/features/chat/ChannelMetadataSettings.tsx` publishes shared changes with
expected revisions and retires requests on group/session/authority replacement.
`ChannelAnnouncement.tsx` owns current details, publication cards and reminders;
`ui/lib/groupAnnouncements.ts` owns bounded device-private per-account version
markers and presentation-only timeline merging. Publication cards never enter
message delivery, read-receipt or ordinary-message unread ledgers.

The [channel guide](help/topics/channels.md#group-announcements) owns the visible
workflow and limits. Cover and announcement updates do not rotate membership or
overwrite each other's revisions. Current-state release cannot become pre-join
message history access.

## Ordinary file custody

`node/group_file_delivery.rs` owns bounded original-sender group-file postal
custody and exact group-offer authority, reusing `opaque_relay.rs`, the existing
attachment producer and `file_consent.rs`. `file_consent_store.rs` isolates signed
revisions by manifest and recipient device without changing its record encoding.
`delivery_runtime.rs` owns private-file custody, while direct receive consent
remains enforced by the existing attachment and guarded-sync owners. Incoming
group completion reservations reuse `delivery_store.rs` and `file_completion.rs`;
mailbox draining may use current group principals without granting private-DM
permission. See [ordinary file custody](network-design-reference.md#ordinary-file-custody).

## Network cost protection

`core/node/network_cost.rs` owns temporary group attachment export eligibility,
independent of signed replica preferences and local retention. Desktop
`src-tauri/src/network_cost/` owns native observations within the installed
runtime's host-task lifetime. Existing group sync projections enforce the policy
in both directions; channel member IPC exposes only local-device status and
session-authorized overrides. See [network cost protection](network-design-reference.md#cost-aware-group-attachment-sharing).

## Public help

`docs/help/` owns the English task manual and stable feature catalog.
`frontend/src/features/help/` renders bundled offline help and resolves explicit
reading-to-settings navigation. `tools/help/` owns shared search, restricted
Markdown rendering, static publication and read-only documentation MCP. These
modules consume no private messaging state and introduce no core protocol
responsibility. See [help system maintenance](help-system.md).

### Account and device ownership

User authority belongs to the account; a device supplies an execution context.
Every consumer must distinguish that authority from current execution
availability. A group owner account is the owner on every certified device,
including a device absent from a legacy physical roster. A member account can
leave from any certified device; an owner account must transfer ownership first
on every device. Public signer validation remains authoritative for actual
protocol effects. An inventory row or display name cannot establish authority.
The canonical channel reducer retains every event in its authorized causal DAG.
It reconstructs account membership proofs only after channel membership controls;
content events cannot change those immutable proofs or bypass control validation.

| Account-owned state or action | Device-owned execution or state |
| --- | --- |
| Login credentials, nickname, avatar and biography | Device signing identity, local storage namespace and storage encryption secrets |
| Private contact remarks, contact state, favorites and portable preferences | Physical paths, OS permissions, autostart and local integration grants |
| Group membership, owner role and permission to administer or leave | Current routes, connections and eligible signers for legacy controls |
| Logical messages, account read state and synchronized private controls | Original event signatures, local media cache and device sharing choices |

Group send, invitation, membership and administration consumers must use the same
account authorization decision. The ability to execute one action locally does
not grant permission to sign another action. In a legacy group, a member device
can send while an owner action waits for an eligible owner signer. Such waiting
must not downgrade the displayed account role, manufacture a grant, or report an
uncommitted effect as successful. A private group reference remains descriptive;
verified source authority and the public channel rules govern execution.

Account-private role observations trust an authenticated account device to report
the canonical result of its full public channel reducer. Receivers verify the
actual source's full signed binding and observation signature. An owner's
adoption of the account requires that owner's independently signed binding.
A member device may relay that binding while retaining its own member role.
Execution always revalidates the actual public membership, lifetime, authority
and signer through the owning channel implementation.
Canonical authenticated `Left` or `Dissolved` observations deny direct enqueue
before WAL or sender-key allocation, even when the local physical public cut is
stale. A fresh verified membership lifetime can supersede an older departure;
positive signer authority still requires the existing public channel checks.

Cross-source reconciliation names exact signed heads by full source identity,
revision and hash. A source attests supersession only after its full public view
contains and resolves the referenced observation's lifetime and authority
decisions. Incomparable observations require reconciliation. Source revisions,
wall clocks, event counts and role ranking cannot establish cross-source
precedence. Replacement heads invalidate superseded observations; bounded
revision floors and terminal observations prevent delayed replay from restoring
obsolete roles. Only real local group observations may be published as a
device's physical source state.

The existing account-document index owns local publication recovery. An incomplete
source head and its exact account, full-device, revision and hash-bound recovery
frontier share one encrypted durable index append. Compaction preserves that
pair; capacity or write failure preserves the previous pair. A required receipt
is retained until an atomic replacement makes it unnecessary. These local index
extensions are excluded from wire metadata and remote ingestion; ordinary index
rows retain their existing JSON representation.

This trust boundary covers private account replicas. A compromised account
device can fabricate private observations; public event and signer validation
remain enforced independently. Account metadata synchronization carries bounded
observations and reconciliation references, so optional history sharing can stay
disabled without exporting message bodies or sender keys to prove role state.

The desktop exposes private group control state through typed account group
interfaces. Generic account document IPC rejects this domain for listing,
metadata, payload reads, writes and removal; it cannot expose operation payloads
or bypass verified control ingestion.
List and member IPC expose the current account role separately from execution
readiness. Their typed projection contains account and device identifiers,
role and readiness, without raw keys or private operation bodies. A failed
observation explicitly requires reconciliation rather than restoring cached
authority.

Account group observations capture signed identity proofs and current privacy
eligibility before acquiring the event-log mutex. The log-held projection uses
only that immutable snapshot: it must not acquire privacy, roster or host-session
locks, including through identity-resolution helpers. Sync projections already
acquire privacy state before the event log; reversing that order can deadlock
both account reconciliation and delivery, then block desktop session queries.

Account-target owner controls share the channel owner's pure preparation rule
with direct mutations. Preparation captures current signed bindings before the
event log, freezes every accepted and invited physical lifetime (including
unresolved bindings), and binds modern targets to their grant incarnation.
Legacy removal closes all certified devices and invitations for the selected
account; account transfer chooses a deterministic accepted device only where the
legacy wire protocol requires one. Existing explicit device-transfer callers
retain their device selection.

The delivery journal appends a new owner-control variant without changing old
bincode discriminants or field layouts. Its structural check binds the original
account input and exact effect. Before replay appends that effect, causal
validation reconstructs its saved parent view and checks the complete witness
and public authority. Current peer labels, later account adoption and later
grants cannot retarget an already prepared effect. Source acceptance preflights
the complete envelope against the existing journal and transaction budgets.

Each witness row binds its physical membership lifetime, acceptance state and
corresponding certified identity together. A valid proof or accepted role from a
different lifetime cannot replace that row. A legacy account leave with a committed
physical leg rolls forward. When another leg acknowledges cancellation during
that turn, the next turn replaces the cancelled leg under the original parent
intent and preserves the committed leg's exact result.

Executor selection requires both a supported operation kind and a current,
executable source view before preferring the local device. A private authority
observation can arrive before its public causal events: a reconciling source
retains the original intent until that public view is available. It must not
interpret missing causal state as an invalid target or exhausted resources,
rewrite the signed executor reference, or allocate a replacement operation ID.

Message actions share a pure source preparation rule and an appended delivery
transaction. Recall selects the original author device, validates the target
group and retained source expiry at first admission, and records the exact local
recall mark before durable terminal completion. Expired intents that have not
entered the effect WAL settle as expired; accepted WAL effects replay their
saved causal view and clock. Startup and running recovery both validate the
source's sealed payload before appending the event and share the recall-sidecar
completion rule. Cancellation and terminal duplicates do not require evicted
target metadata. Missing expiry evidence never means verified absence of expiry.

The existing reaction projection owns private explicit group-action copies,
retaining original full source identity and public effect IDs independently of
optional archive download. Its explicit `MTREAC` to `MTREA2` migration preserves
legacy DM records through encrypted, synced, atomic replacement. Channel rows
share the same compaction and pruning owner, with an 8 MiB live-byte budget and
100,000 distinct source/target/emoji keys; failed admission never evicts an active
projection. Stored effects retain their original physical author and signature.
Views reduce certified siblings under one account principal, so a sibling can
remove the account’s existing reaction without adding a second reacting member.
Modern public effects use their causal account binding; private explicit copies
retain the authenticated executor certificate in an appended record variant.
Unknown or ambiguous authors remain separate device principals until verified
evidence arrives. Device adoption must not reassign an earlier account’s action,
and duplicate public/private copies must not increase the reaction count.

The peer directory preserves account-transition ambiguity in its version-three
snapshot. Earlier snapshots cannot prove which account owned an unbound
historical action; their loaded identities remain ambiguous for that fallback.
This does not revoke current device certificates or override an effect's saved
causal binding. Fresh version-three records distinguish known unchanged bindings
from old snapshots that already discarded an earlier account.

Explicit group files reuse the existing file preparation, delivery journal and
account attachment workers. One original operation retains its signed manifest,
random file identity and ciphertext through retries and executor changes in
availability. A source may accept execution only after verified ciphertext is
durable; optional archive and attachment preferences do not suppress the
account's explicitly requested send. Existing recipient consent, expiry, recall
and erase rules continue to govern file release.
The operation journal retains the original result descriptor and a monotonic
local completion marker independently of rotating source heads. Unfinished
preparation pins its existing cache; completed copies may be reclaimed by the
ordinary file owner without automatically downloading them again. This metadata
does not create another queue, transport or public membership grant.

Modern invitation acceptance and account departure share pure preparation and
causal delivery replay with direct channel operations. Their appended journal
envelope preserves old transaction encodings and binds the original account
intent to its exact grant incarnation. A valid departure retains only the
original source's bounded public retry route; rejoining cannot revive an older
departure route. A revoked grant continues to project the account as left even
when an offline sibling still advertises an older member observation.

Account invitations share their existing signed invitation, authority grant and
optional encrypted history batch with direct callers. Capability verification
and history preparation use the existing bounded account data workers before
membership and delivery guards. The appended invitation envelope binds its
original target account, full device proof, grant incarnation and causal parent
view; its request identity selects the first invitation event, while recovery
retains every event in the unchanged batch.

Explicit invitation refusal uses a versioned nonempty `ChannelLeave` payload
bound to one pending grant incarnation. Existing empty acceptance and departure
encodings remain unchanged. Older clients ignore this new refusal until they
upgrade. A refusal reserves the acceptance epoch as well as revoking the grant,
so a concurrent acceptance cannot reuse keys exposed by a partial accepted view.
That reservation never activates membership or emits acceptance or content keys.
Observed acceptance prevents a later refusal; a later invitation with a different
incarnation remains independent.

Directory reconciliation and observed history policy are descriptive metadata,
separate from account permission and execution readiness. Reference list/member
projections preserve the last coherent source observation, disclose unresolved
reconciliation, and mark a missing policy as unknown instead of asserting a
local default. A policy observation cannot authorize archive or attachment
release; those owners recheck current membership and disclosure policy.

Group creation retains its existing public creation and invitation producer.
Its owner-scoped desktop adapter checks the expected account and exact runtime
session before resolving invitees and rechecks at the creation commit. Both
desktop creation adapters take account IDs, not device IDs. The existing
`channels/account_invitation.rs` owner authenticates the selected account and
compatible format over Noise, trying its known routes. Creation reuses that
resolver and the existing account-authority grants, including one pending grant
per invited account and an immediately accepted creator-account owner grant.
The existing atomic new-channel event-store boundary publishes genesis, account
authority and initial history policy together; interrupted creation cannot expose
a device-only genesis while losing its account grants.
It does not invent an existing group lifetime or operation reference before genesis.

Typed desktop operation commands capture the exact local session owner and
expected account. Durable submission and cancellation recheck that authority at
the core admission callback; lifecycle reads retain the matching runtime guard.
The guarded ID factory binds an opaque ID to the full origin device and a random
nonce without accepting work. Deduplication still belongs to the account and ID;
a sibling resolves the original reference before resuming a foreign-origin ID.
Submission rechecks the existing entry under the final journal write lock after
transport preflight: an authenticated result may arrive during preflight. A
matching retry returns that retained status; a mismatched group or digest still
rejects the ID, and lookup admission never replaces an adopted result.
Logical operation IDs remain separate from committed public event IDs. Bounded
list and status projections contain lifecycle metadata only. The frontend IPC
adapter preserves pending outcomes for the existing action and outgoing-intent
owners; acceptance alone does not finish a dialog or mark a message as sent.

Simple owner controls prepare their exact signed public effect from the same
locked canonical membership projection used to revalidate the pinned source.
Their original logical operation, request digest and full executor identity bind
the existing delivery WAL; durable public effects and sidecars precede the
immutable completion result. A canonical no-op still requires a durable parent
view and never invents a public event ID. Affected source access and inventory
publish through their existing signed owners before public fanout can wait on a
slow member.

Authenticated body-free sibling invalidations remain active independently of
optional content categories. Mandatory control requests and results reuse that
bounded wake and account-document channel; content transfer still applies its
own category, sharing and read-entitlement checks. The existing two mandatory
workers reserve one slot for GroupControl and one for group inventory/device
names. Each lane retains its own full-peer-identity cursor, partial page, refresh
and retry state. A held directory response cannot delay an explicit operation
request or result; control traffic cannot consume the directory's reserved slot.
Combined progress is up to date only when both lanes have settled, and an error
or required upgrade in either lane remains visible. The shared TCP budget and
authentication, category and authority checks remain unchanged. The
[network contract](network-design-reference.md#g-linking-and-ongoing-account-synchronization)
owns the distinction between an accepted account's private copy and public
physical membership.

The other two existing synchronization slots are partitioned between optional
private documents and history/favorites. A held history response cannot delay an
authorized custody confirmation, profile or preference document. History and
favorites preserve their own continuations and coalesced refreshes; their errors
do not replace document progress. Optional document category and entitlement
checks still apply. Together with control and directory work, these remain four
workers in the same shared session pool, not additional network capacity.

### Account profiles and private trust

`identity/account.rs` and `identity/account_keystore.rs` own immutable creation
metadata persisted with the account key. Pairing and recovery preserve it without
inferring missing dates. `node/account_profile_data.rs` owns signed bounded
ordinary profile data and current-profile persistence; `node/account_profile_io.rs`
owns admitted edits, authenticated reception and legacy compatibility.
`node/profile_receive_work.rs` owns account-scoped exact receive evidence and
retry scheduling on the profile's encrypted SQL connection. Discovery advances
through at most 128 committed event headers; each retry pass selects 16 fixed
metadata rows before loading one signed body at a time. Unknown devices and
disabled sibling-profile synchronization remain pending. Current-profile writes
finish before processed evidence is committed, so a crash or storage failure
retries idempotently. No historical profile-ID set is rebuilt in memory.
Send preparation enters host authority before acquiring the delivery journal.
Desktop biography saves reply after durable local acceptance while admitted fanout
continues independently. The ordinary nickname and bio edit share the publication
mutex, preventing a stale bio save from undoing a rename.
Profile-store guards end before delivery preparation acquires channel, routing or
event-log state. Group inventory first snapshots channel facts, releases the channel
book, then resolves profile names and routing classifications. Neither side may
retain one of these guards while waiting for the other. Deterministic lock-boundary
regressions cover background publication, biography/avatar edits and inventory
projection; keep them in both affected validation selections.
`src-tauri/src/account_profile.rs` exposes reads and bio edits, with no caller-supplied
names, timestamps or public proofs. `src-tauri/src/trust.rs` owns private observation
history and manual verification. Frontend `store/accountProfiles.ts` shares
session-bound profile reads and live invalidations across the header and dialogs.
The [profile and verification guide](peer-trust.md) owns meanings and evidence limits.

## Google Drive recovery transport

`src-tauri/src/google_drive/` owns remembered password-encrypted local OAuth
refresh grants, ephemeral operation connections, exact-session admission, bounded
Drive HTTP transfers and temporary encrypted file lifetimes. Its `remembered.rs`
keeps Google credentials native and separate from recovery archives.
`src-tauri/src/recovery.rs` remains the owner of consistent snapshot/runtime restart,
archive verification and transactional profile restore. Its `recovery/setup.rs`
owns the reusable local recovery key, one-time acknowledgement and selected
destination, using the existing password-encrypted local-user storage and exact
session/lock authority. `core/recovery/` owns format
and cryptography. `ui/lib/api/googleDrive.ts` adapts IPC, and
`ui/features/recovery/` provides shared connection/progress presentation for the
existing export/restore dialogs. See [backup and recovery](backup-recovery.md#google-drive-security-and-boundaries).

## Bounded history for channel invitations

`channel/history.rs` owns the four fixed history policy presets. The existing
channel lifecycle in `node/channels.rs` owns invitations; `node/join_history.rs`
owns frozen encrypted copies, owner-policy validation and post-acceptance row-key
release. It reuses the delivery journal, channel crypto primitives, causal
membership projection and received-history store. Desktop IPC delegates authority
to core; the member dialog presents the policy without independently deciding
history eligibility. The [channel guide](help/topics/channels.md#history-for-new-members)
owns user-visible limits and availability.

Native message presentation is queued onto the UI thread before acquiring the
exact desktop-session guard. The queued callback revalidates its session and runtime
ticket before emitting private events or notifications. A worker must not hold that
guard while synchronously querying window focus: the UI thread may itself be waiting
for the same session. `events.rs` owns this dispatch boundary and its lock-order and
stale-session regressions; `runtime_bootstrap.rs` uses it for DM, group and file callbacks.

### Account document projections

`node/account_documents/` owns private typed metadata and blobs, not group
authority or public identity. The existing account worker and session carry
manifest pages and bounded chunks; there is no second network daemon. Native
`account_documents.rs` checks exact local owner, runtime account and version
before mutations or plaintext release. Frontend linked coordinators project
accepted records into the owning draft, reception, pack and avatar stores; they
never own another network synchronization loop. Pack validation remains in
`pack.ts`; pack storage is namespaced by local owner and cryptographic account.
`settings/shared.rs` owns the allowlist mapping of portable settings onto existing
per-key account preferences. Device paths, system permissions, autostart and
local-agent grants stay outside the replicated allowlist.
`account_documents/controls.rs` owns authenticated private DM control publication,
original-action ordering and sibling projections. `reactions.rs` and `recall.rs`
publish accepted actions through it; generic native document mutations cannot
create control records. Explicit kept-source preparation validates legacy caches.
`account_sync/archive.rs` filters known recalled private bodies/cards and invalidates cached
exports when the control revision changes. Group authority and local deletion
remain with their existing owners.

`channel_custody.rs` owns locally observed exact-event possession and retractions;
its indexed child keeps those exact positive and negative observations in the
leased profile database, with selected-event reads and 32-row keyset migration
pages. It shares the history-source publisher and legacy-writer barrier rather
than retaining the custody journal in memory. Auxiliary source identities 16–18
use a separate registry without rewriting the installed transcript-source schema.
`account_documents/custody.rs` owns their account-private versioned publication
and sibling projection. It reuses certified account sessions and document digests,
without adding a group transport or claiming recipient delivery. Channel authority
determines eligible external member principals. The footer distinguishes missing
status from a known zero count; its status coordinator clears stale projections
only for the messages and runtime lifetime it requested.
The document store keeps a separate bounded custody-key quota and an owned-file
scan budget consistent with legal document counts. `media_store.rs` prunes the
enumerated unreferenced regular files in one pass, preserving live files and
synchronizing the parent directory before successful return.

`account_documents/public_profile.rs` relays original signed contact profiles under
Profile consent; `account_profile_io.rs` retains signature, revision and avatar
projection authority. Per-source document heads preserve each contact's original
profile, with bounded optional publication repair that cannot block login.
`account_documents/group_activity.rs` owns History-gated source-signed control
observations tied to the selected inventory snapshot. It cannot install membership
or keys. Both reuse the existing document worker and certified sibling sessions.

`account_documents/inventory.rs` owns source-certified descriptive group snapshots,
bounded automatic publication and account-private reference projection. It reuses
the document version/digest owner and private account worker. The group directory
is mandatory account topology; optional history, profile, appearance and contact
data retain their existing policies. A source reads real local group state and
never republishes a borrowed reference as authority. The native channel adapter
may project reference member facts for display under runtime admission; group
authorization, keys and mutations remain exclusively with the existing channel
owners. The frontend distinguishes this read-only reference from a backup archive
and removes its explanation when real validated group state arrives.


`node/account_documents/device_hostname.rs` owns bounded private per-device hostname
publication and certified-roster projection. The desktop runtime bootstrap reads
OS hostnames; admitted sync IPC releases them only for the expected current account.
`PersonalDevices` renders them without changing identity, account nicknames or
presence semantics. Hostnames reuse encrypted account-document persistence and the
existing certified sibling transport; public discovery and profiles never carry
them. See [private account synchronization](network-design-reference.md#g-linking-and-ongoing-account-synchronization).
