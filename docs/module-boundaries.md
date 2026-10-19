# Module Ownership and Reuse

This is the implementation ownership map for maintaining Mesh Talk and assigning
parallel work. Use it to locate the owner of a decision before adding another
implementation. Network semantics and configuration belong to the
[communication architecture](network-design-reference.md); this document defines
where those decisions live in code and how to change them safely.

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
private IPC; `crates/mesh-talk-mcp` owns SDK/stdio translation only. Core
`node/outgoing_message.rs` owns retry-safe event acceptance and sender-state WAL
recovery. Desktop `src-tauri/src/integrations.rs` binds exact-session leases and
graceful listener retirement. See [bot integration setup](bot-integrations.md).

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
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Participant lifecycle                          | `core/node/participant_runtime.rs`, `core/node/runtime.rs`, `core/bin/mesh-talk-node.rs`                                                                                        | Reuse prepared/running participant ownership for common network tasks. Desktop retains profile/session bootstrap; CLI retains its data paths, REPL and post-office mode. Adopt host producers and blocking file saves into retirement. |
| Device/account identity                        | `core/identity/`, `core/node/account_devices.rs`, `core/node/peer_directory.rs`                                                                                                 | Use verified bindings; never infer authority from names, IPs or sidebar entries.                                                                                                                                                       |
| Discovery and reachability                     | `core/discovery/`, `core/node/privacy_routes.rs`, `privacy_routing.rs`                                                                                                          | Transport route verification stays in core; visibility and reachability remain different facts.                                                                                                                                        |
| Desktop VPN inventory                          | `src-tauri/src/routed_discovery.rs`, `tailscale_discovery.rs`, `easytier_discovery.rs`                                                                                          | Enumerate optional existing clients concurrently; bound, interleave and deduplicate candidates. Core retains signed probing, privacy gates and identity/route verification.                                                            |
| Peer presentation and infrastructure filtering | `ui/lib/rosterProjection.ts`, `contactVisibility.ts`, `postOfficeOptions.ts`                                                                                                    | Reuse account/device/role lookups and role-specific selectors. Diagnostics retain infrastructure; people pickers exclude it.                                                                                                           |
| Direct messaging                               | `core/node/dm.rs`, `dm_envelope.rs`, `dm_ratchet.rs`                                                                                                                            | Encrypt/address in core; UI creates an intent and renders its result.                                                                                                                                                                  |
| Channels and authority                         | `core/node/channel.rs`, `channels.rs`, `channel_senders.rs`                                                                                                                     | Signed membership is authoritative. Do not derive membership or permissions from a filtered contact list.                                                                                                                              |
| Group recovery/custody                         | `core/node/channel_custody.rs`, `channel_projection_cache.rs`, `session.rs`                                                                                                     | Reuse accepted membership and retained-event reconciliation; availability is not an unconditional delivery guarantee.                                                                                                                  |
| Durable outgoing delivery                      | `core/node/delivery_store.rs`, `delivery_runtime.rs`, `delivery_receipt.rs`                                                                                                     | Storage owns persisted transitions; runtime owns bounded direct/relay progress and scheduling; receipts own exact completion evidence.                                                                                                 |
| Post-office routing/federation                 | `core/postoffice/`, `core/node/postbox.rs`, `opaque_relay.rs`, `bridge_directory.rs`, `bridge_client.rs`, `relay_policy.rs`                                                     | Reuse relay eligibility, opaque projection and incremental mailbox admission; directory participation does not grant relay or conversation authority.                                                                                  |
| Transport/session reuse                        | `core/transport/`, `core/node/transport.rs`, `session_pool.rs`, `session.rs`                                                                                                    | Reuse authenticated bounded sessions; feature modules do not open ad hoc sockets or create their own retry loops.                                                                                                                      |
| Files/folders                                  | `core/file/`, `core/node/files.rs`, `filebook.rs`, `file_consent.rs`, `file_completion.rs`                                                                                      | Consent, chunk integrity and recipient completion remain distinct transitions. Presentation intake belongs to `ui/features/chat/attachments/useAttachmentIntake.ts`; the store retains intent acceptance/retry.                        |
| History/search/recall/reactions                | `core/node/queries.rs`, `search.rs`, `recall.rs`, `reactions.rs`                                                                                                                | Query through core; frontend uses verified account bindings and centralized contact visibility.                                                                                                                                        |
| Linking and sibling replay                     | `core/node/pairing.rs`, `linking.rs`, `account_sync*.rs`, `account_preferences.rs`                                                                                              | Pairing authority and replay policy stay separate from the dialog. Host adoption belongs to `src-tauri/src/linked_account.rs`.                                                                                                         |
| Calls                                          | `core/node/call.rs`, `ui/store/calls.ts`, `ui/features/chat/WebRtcTestDialog.tsx`                                                                                               | Core owns authenticated signaling; the call store owns call lifecycle and WebRTC media; the test dialog owns local media diagnostics. Do not reuse mailbox custody as live signaling.                                                  |
| Auth/runtime ownership                         | `src-tauri/src/state.rs`, `chat_commands/state.rs`, `runtime_bootstrap.rs`, `runtime_admission.rs`, `linked_account.rs`, `owner_commands.rs`                                    | Reuse session leases and runtime installation checks; commands adapt inputs and errors. Frontend work uses `ui/store/ownership.ts`.                                                                                                    |
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
  retain best-effort fanout; portable recovery snapshots do not include them.
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
- Peer-avatar and language-profile journals reclaim obsolete versions using
  encrypted record-log rewrites. They compact on opening older redundant logs
  and amortize subsequent rewrites by profile-version count, independently of
  immutable receive-decision count. Latest clear tombstones and every retained
  language decision survive compaction; the budget is relative to live profiles,
  rather than a cap on the number of distinct accounts.
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
been eliminated. The historical [boundary audit](evals/architecture-boundaries-2026-10-15.md)
records earlier findings and fixes; this file is the maintained ownership guide.

## Public help

`docs/help/` owns the English task manual and stable feature catalog.
`frontend/src/features/help/` renders bundled offline help and resolves explicit
reading-to-settings navigation. `tools/help/` owns shared search, restricted
Markdown rendering, static publication and read-only documentation MCP. These
modules consume no private messaging state and introduce no core protocol
responsibility. See [help system maintenance](help-system.md).
