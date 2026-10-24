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
request schemas derive from the existing models. The General settings UI uses
the shared save coordinator and an explicit-owner typed adapter. See
[desktop local access](bot-integrations.md#desktop-local-agent-access).

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
| Files/folders                                  | `core/file/`, `core/node/files.rs`, `filebook.rs`, `file_consent.rs`, `file_completion.rs`                                                                                      | Consent, chunk integrity and recipient completion remain distinct transitions. Presentation intake belongs to `ui/features/chat/attachments/useAttachmentIntake.ts`; the store retains intent acceptance/retry.                        |
| History/search/recall/reactions                | `core/node/queries.rs`, `search.rs`, `recall.rs`, `reactions.rs`                                                                                                                | Query through core; [recent-search references](ARCHITECTURE.md#recent-search-references) reuse canonical history identity and coverage. Frontend uses verified account bindings and centralized contact visibility.                                                                                                                                        |
| Linking and sibling replay                     | `core/node/pairing.rs`, `linking.rs`, `enrollment.rs`, `account_sync*.rs`, `account_preferences.rs`, `account_access.rs`                                                                           | Pairing authority and replay policy stay separate from the dialog. Host adoption belongs to `src-tauri/src/linked_account.rs`; signed-out enrollment belongs to `device_enrollment.rs` and reuses the recovery publication journal; private login application belongs to `shared_account_login.rs`.                                                                                            |
| Calls                                          | `core/node/call.rs`, `ui/store/calls.ts`, `ui/features/chat/WebRtcTestDialog.tsx`                                                                                               | Core owns authenticated signaling; the call store owns call lifecycle and WebRTC media; the test dialog owns local media diagnostics. Do not reuse mailbox custody as live signaling.                                                  |
| Auth/runtime ownership                         | `src-tauri/src/state.rs`, `chat_commands/state.rs`, `runtime_bootstrap.rs`, `runtime_admission.rs`, `linked_account.rs`, `owner_commands.rs`, `owner_commands/search.rs`                                    | Reuse session leases and runtime installation checks; commands adapt inputs and errors. Frontend work uses `ui/store/ownership.ts`.                                                                                                    |
| Message UI transitions                         | `ui/store/chat.ts`, `outgoingIntent.ts`, `conversationState.ts`, `chatRuntimeEffects.ts`                                                                                        | Intents own acceptance, conversation state owns cache transitions, effects bridge feature stores. Do not settle messages independently in components.                                                                                  |
| Settings/privacy                               | `src-tauri/src/settings.rs`, `privacy_commands.rs`, `ui/store/settings.ts`, `privacy.ts`                                                                                        | `ui/lib/privacySettings.ts` owns presentation defaults/projection; `settingsDialogSession.ts` owns load/save generations. Network admission remains in core.                                                                           |
| Contact hiding                                 | `src-tauri/src/contact_policy.rs`, `ui/store/contactPolicy.ts`, `ui/lib/contactVisibility.ts`                                                                                   | A presentation filter, not a communication block. Reuse it for search and pickers; preserve shared groups and incoming delivery.                                                                                                       |
| Backup/recovery                                | `core/recovery/`, `core/node/recovery.rs`, `src-tauri/src/recovery.rs`                                                                                                          | Core validates archive/snapshot; host owns staging/publication and runtime barriers; UI presents inspection and confirmation.                                                                                                          |
| Appearance/avatars/packs                       | `ui/store/avatars.ts`, `packs.ts`, `ui/lib/theme.ts`, `pack.ts`, `ui/components/identity/`                                                                                      | Reuse identity rendering and pack validation. Do not create feature-specific avatar stores or variant parsers.                                                                                                                         |
| Diagnostics/updates/native controls            | `src-tauri/src/diagnostics.rs`, `updates.rs`, `window_geometry.rs`, `window_visibility.rs`, `ui/features/chat/DiagnosticsDialog.tsx`, `diagnostics/useDiagnosticsController.ts` | Keep OS operations in host. Live views reuse `useSerialPoll.ts`; diagnostic observations are not policy authority.                                                                                                                     |

These owners already exist. Adding a new abstraction is justified by a shared
business decision or a real lifecycle seam, not merely by similar syntax. For
example, discovery TTL and conversation presence need not share a constant just
because both measure time; session admission and mailbox admission must retain
their distinct authorization contracts.

### Coordinated state owners

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
coverage cannot count one logical message twice. Independent messages with equal
text retain separate IDs and counts. Legacy providers without a logical identity
retain their compatibility counter path.

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

## 6. Remaining opportunities and limits

- Settings categories now live in `features/chat/settings/`, composed by
  `SettingsDialog.tsx`. `useSettingsController.ts` shares acknowledged values and
  delegates owner/view validity to `settingsDialogSession.ts`; `model.ts` owns
  dependency reasons and retention tightening. See [settings](settings.md).
  Future settings must extend the relevant category and shared rules rather than
  adding another independent save coordinator.
- `ConversationView.tsx` delegates picker, drop, paste and screenshot intake to
  `attachments/useAttachmentIntake.ts`. Acquisition owns conversation/runtime
  leases, staging cleanup and source-specific media intent; store intents retain
  accepted paths and retry ownership. Screenshot edits retain their originating
  conversation rather than targeting whichever conversation is active later.
  Confirmed screenshots join the composer's pending files; only the normal Send
  action hands them to delivery. `screenshot/` owns image-coordinate geometry,
  annotation painting, floating controls and temporary fullscreen restoration.
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
- Archived channel summaries and admission use `ChannelBook::projected_metadata`
  and its signed control reducer. Key distribution/decryption remains separate;
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

## Public help

`docs/help/` owns the English task manual and stable feature catalog.
`frontend/src/features/help/` renders bundled offline help and resolves explicit
reading-to-settings navigation. `tools/help/` owns shared search, restricted
Markdown rendering, static publication and read-only documentation MCP. These
modules consume no private messaging state and introduce no core protocol
responsibility. See [help system maintenance](help-system.md).

### Account profiles and private trust

`identity/account.rs` and `identity/account_keystore.rs` own immutable creation
metadata persisted with the account key. Pairing and recovery preserve it without
inferring missing dates. `node/account_profile_data.rs` owns signed bounded
ordinary profile data and current-profile persistence; `node/account_profile_io.rs`
owns admitted edits, authenticated reception and legacy compatibility.
Send preparation enters host authority before acquiring the delivery journal.
Desktop biography saves reply after durable local acceptance while admitted fanout
continues independently. The ordinary nickname and bio edit share the publication
mutex, preventing a stale bio save from undoing a rename.
`src-tauri/src/account_profile.rs` exposes reads and bio edits, with no caller-supplied
names, timestamps or public proofs. `src-tauri/src/trust.rs` owns private observation
history and manual verification. Frontend `store/accountProfiles.ts` shares
session-bound profile reads and live invalidations across the header and dialogs.
The [profile and verification guide](peer-trust.md) owns meanings and evidence limits.

## Google Drive recovery transport

`src-tauri/src/google_drive/` owns ephemeral desktop OAuth grants, exact-session
admission, bounded Drive HTTP transfers and temporary encrypted file lifetimes.
`src-tauri/src/recovery.rs` remains the owner of consistent snapshot/runtime restart,
archive verification and transactional profile restore; `core/recovery/` owns format
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
