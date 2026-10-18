# Personal Messaging and Cross-Device Synchronization Implementation Plan

> **For agentic workers:** Use `superpowers:executing-plans` to execute approved
> work packages in sequence. Each package should be reviewed and committed before
> starting the next. No application implementation is authorized by this document.

**Goal:** Make messages and files synchronize reliably across trusted devices and
authorized groups, including personal messaging, while preserving existing behavior.

**Architecture:** Extend the existing account-addressed DM, delivery WAL and bounded
account-history reconciliation. Add explicit policy decisions and versioned typed
private records, keeping group authorization and account authority separate.

**Tech stack:** Rust/Tokio, existing encrypted record logs and Noise transport,
Tauri IPC, React/TypeScript/Zustand, current Rust and Vitest/Playwright tooling.

**Status:** Proposed work packages, based on code inspection at `762525d` on
2026-10-18; no steps below have been implemented or executed. This is an
architecture-level implementation plan, not a drop-in patch. Interface sketches
describe proposed additions rather than existing callable APIs. The
[design](multi-device-sync-design.md) defines behavior and security boundaries;
the [research](research/multi-device-sync-patterns.md) records source evidence.
The plan lives here because repository instructions reserve `docs/superpowers/`
and root `plans/` for local, untracked work.

## Scope, order and completion boundaries

**Confirmed first-release scope: messages and files.** Work packages 1–4 are all
required: local/offline personal messages, consistent selection, Saved Messages,
and personal files with optional byte downloads. Validate messages and files in
self-only, group-only and combined scenarios. A text-only milestone does not
complete this release.

Packages 5–6 describe optional future application state and are outside the
confirmed implementation scope. Preserve existing preference/profile behavior;
do not expand its scope as part of this release. Packages 7–8
cover stronger availability and identity-preserving device management as separate
protocol/security projects. Package 9 defines the release gate for each selected
milestone; the first release need not wait for the authority migration, but cannot
claim its security guarantees.

No new CRDT, central account server, generic file synchronization or wholesale
database copying is required. Keep all accepted work on the user's optimization
branch, with one focused conventional commit per completed slice. Never commit
implementation merely to satisfy a plan checkbox before validation passes.

## Owner and file map

| Boundary                       | Existing owners to extend                                                                                                                                                       | Proposed focused additions                                                                                                           |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| Personal acceptance and replay | `crates/mesh-talk-core/src/node/{dm.rs,delivery_store.rs,delivery_runtime.rs,queries.rs,conversation.rs}`                                                                       | `node/personal_messages.rs` only if zero-destination acceptance cannot stay cohesive in `dm.rs`                                      |
| Sync policy and lifecycle      | `node/{account_sync_policy.rs,account_sync.rs,linking.rs,serving.rs,files.rs}`                                                                                                  | `node/account_sync/selection.rs`: pure boundary decisions                                                                            |
| Versioned private state        | `node/{account_sync_wire.rs,account_preferences.rs,node.rs,runtime.rs}`                                                                                                         | `node/private_records.rs`, `node/private_records_wire.rs`: codec/store and negotiated frames                                         |
| Private read/draft projection  | `node/{queries.rs,account_sync.rs}` and existing history import owners                                                                                                          | `node/private_read_state.rs`, `node/private_drafts.rs`: domain-specific merge and projection                                         |
| File routes                    | `node/{files.rs,account_file_proofs.rs,account_sync.rs,attachment_consent.rs,file_completion.rs}`                                                                               | A versioned personal file-route proof alongside existing manifest proof                                                              |
| Host adapters                  | `src-tauri/src/chat_commands/{messaging.rs,sync.rs,account.rs}`, `src-tauri/src/{linked_account.rs,lib.rs}`                                                                     | Commands for private state/progress only after core contracts exist                                                                  |
| Frontend                       | `frontend/src/{store/chat.ts,store/outgoingIntent.ts,lib/types.ts,lib/api/chat.ts,lib/unreadStorage.ts,lib/linkedPreferencePublisher.ts}`, chat sidebar/linking/settings owners | `features/chat/sidebar/PersonalConversation.tsx`, `lib/privateReadState.ts`, `lib/privateDrafts.ts` when those packages are approved |
| Documentation/evaluation       | `docs/{ARCHITECTURE.md,network-design-reference.md,settings.md,backup-recovery.md,module-boundaries.md}`, `docs/evals/linked-device-sync-ai-eval-suite.json`                    | Focused scenarios/specs named below                                                                                                  |

Paths prefixed `node/` above are relative to `crates/mesh-talk-core/src/`. Register
new Rust modules and runtime handles in the existing owner modules, and keep the
core UI-free. This map is a set of intended seams, not a request for unrelated
file restructuring.

## Work package 1: durable self messaging without another device

**Primary files:** core `node/dm.rs`, `delivery_store.rs`, `delivery_runtime.rs`,
`queries.rs`, `node_tests.rs`; new inline tests next to any added acceptance logic.

- [ ] Add `personal_message_is_durable_without_siblings` to construct one account
      with an empty routing roster, enqueue text to its own account, assert exactly
      one `from_me` history row, reopen its private stores, and assert the same ID/text.
- [ ] Add failure/replay cases: failed durable append returns failure without a
      visible accepted row; accepted WAL followed by projection failure replays once;
      user retry preserves the existing outgoing-intent ID semantics.
- [ ] Run each new test by its full name through `make test-focused` and observe
      the intended initial failure (`UnknownPeer` for the zero-sibling case).
- [ ] Add a self-target branch before remote-contact initiation. Derive the
      conversation from the explicit target, keep a stable logical `msg_id`, and
      commit the sent envelope through the existing durable transaction/replay owner.
      Define empty destinations as valid only for a self-account message. Audit every
      `destinations[0]` consumer and outgoing-log validation; do not put a dummy peer
      or a self-ratchet into the destination list.

```text
accept_account_message(target, envelope):
  if target == own_account:
    conversation = account_conversation_id(own_account, own_account)
    destinations = eligible_known_siblings()  // may be empty
  else:
    preserve_current_remote_target_validation()
  wal.accept(logical_message, conversation, destinations, local_sent_projection)
  notify_account_reconciliation_after_acceptance()
```

- [ ] Attach a later sibling through normal linking and show the existing record
      appears once without rewriting the logical ID. Repeat with two siblings and a
      simultaneous reconnect; preserve existing deduplication tests.
- [ ] Verify existing loopback test `send_to_account_self_syncs_to_own_other_device`
      and ordinary account DM send/recovery. Commit `feat(sync): persist personal
messages without a linked peer` after all selected tests pass.

**Development commands:**

```bash
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=personal_message
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=send_to_account_self_syncs_to_own_other_device
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=send_to_account_delivers_to_a_peer_account_over_loopback
```

Expected: nonzero matching test counts, no ignored replacement for the required
loopback assertions, all selected tests pass. These commands are planned, not
executed evidence for this research task.

## Work package 2: one selection contract across account paths

**Primary files:** core `node/account_sync_policy.rs`, new
`node/account_sync/selection.rs`, `account_sync.rs`, `dm.rs`, `delivery_runtime.rs`,
`linking.rs`, `files.rs`, `profile_io.rs`; inline policy tests and `node_tests.rs`.

- [ ] Define a pure decision input with current local policy, validated peer role,
      category, operation boundary and conversation authorization. Unknown categories
      fail closed. Group live traffic remains governed by group membership, not the
      linked-history switch. Preserve profile's existing signed path.
- [ ] Add a table-driven matrix covering source on/off × receiver on/off ×
      sibling/outsider × bootstrap/live/backfill/attachment. Verify disabled account
      replication never becomes an excuse to block ordinary outgoing DMs to others.
- [ ] Apply source policy before new sibling fan-out and receiver policy before
      plaintext projection. Handle ratchet/event ingestion carefully: consume required
      cryptographic/control state without incorrectly marking skipped content imported.
      Ensure re-enabling can recover skipped retained content from an eligible sibling.
      Persist a minimal retry/suppression decision if replay would otherwise resurrect
      disabled content locally. Document whether ciphertext remains in the event log;
      exclusion is not a claim of zero encrypted bytes received.
- [ ] Recheck policy/session generation immediately before each durable import or
      blob commit. Source-side changes during a response cancel/restart affected
      export work as well. Keep existing off/on ABA protections.
- [ ] Extend pairing negotiation to express requested history before streaming it.
      Do not promise “no history transferred” for legacy pairing that transmits a
      bootstrap which the receiver later declines to import. Show the limited legacy
      behavior or require upgrade for strict no-transfer enrollment.
- [ ] Add restart and retry tests for disabled live copies, disable/re-enable
      mid-page, disabled-at-link, no stale digest acknowledgement, and control records
      unaffected by content filters. Commit `fix(sync): enforce category policy across
sibling replication` after focused tests pass.

```bash
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=node::account_sync_policy
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=node::account_sync::tests
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=linking_transfers_the_account_secret_with_a_valid_code
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=disabled_profile_sync_defers_sibling_avatar_until_enabled
```

## Work package 3: a permanent Saved Messages conversation

**Primary files:** frontend sidebar/conversation-list owners, `store/chat.ts`,
`store/outgoingIntent.ts`, `lib/types.ts`, API adapter if necessary, relevant locale
JSON files; existing `Sidebar.test.ts`, new `sidebar/personalConversation.test.ts`;
host messaging/history commands only if an explicit personal API is needed.

- [ ] Build a personal row from the current runtime account ID. Reuse conversation
      kind `account` and the self account ID rather than inventing a second transport
      route. Keep the entry outside ordinary contact hide/delete and peer-count rules.
- [ ] Test zero peers, first login, expired discovery, hidden contacts, account
      adoption, logout and account-switch during an async history request. Assert one
      personal entry and no data from the retired owner/runtime.
- [ ] Connect ordinary compose/history/reply/sticker actions to the durable path
      from package 1. Preserve optimistic-intent retry/deduplication. Do not add a
      person-contact privacy grant merely to save a personal note.
- [ ] Add accurate local/sibling status labels. Separate current selected-category
      progress from recipient delivery receipts. Avoid showing personal incoming
      copies as a new external person's unread notification by default.
- [ ] Extend `frontend/e2e/chat.spec.ts` or create
      `frontend/e2e/personal-messages.spec.ts` for visible entry, offline save, reopen
      and keyboard navigation. Run only the affected Vitest files during development;
      reserve the selected Playwright specs for the PR/release gate.
- [ ] Update implemented documentation only after behavior exists. Commit
      `feat(chat): expose a permanent Saved Messages conversation`.

```bash
cd frontend
npm test -- src/features/chat/Sidebar.test.ts src/features/chat/sidebar/personalConversation.test.ts
```

## Work package 4: personal files and honest progress

**Primary files:** core `node/files.rs`, `account_file_proofs.rs`, `account_sync.rs`,
`account_sync_wire.rs`, `account_sync/page.rs`, `attachment_consent.rs`,
`file_completion.rs`; host sync IPC; frontend `LinkDeviceDialog.tsx`, file/history
projections and typed APIs.

- [ ] Negotiate extended file-card/route capabilities using a new protocol
      identifier, freezing legacy bincode layouts. Specify canonical proof bytes
      binding account, personal host conversation, original manifest ID/hash and author
      certificate. Add round-trip and old-decoder fixtures before enabling writes.
- [ ] Permit self-hosted cards only when that authenticated binding validates.
      Include the personal host in export/attachment lookup, remove raw-device fallback
      for negotiated personal routes, and preserve the old fallback for legacy input.
      Do not loosen unrelated-account or channel-membership checks.
- [ ] Persist card metadata offline even with no sibling. Retain the local blob
      in the encrypted managed store; do not expose an absolute local source path to
      another device. Define transaction recovery across manifest/card/blob commits.
- [ ] On upgraded peers, separate history/file metadata from optional blob download.
      Map old attachment policy conservatively and surface legacy file-card limits;
      do not silently reinterpret old peers as supporting metadata-only history.
- [ ] Test three devices with one attachment-off device, source file removed after
      acceptance, source offline, interrupted transfer, tampered hash/route, unrelated
      account request, size overflow, local clear, and restart before blob commit.
- [ ] Extend per-device/per-category status with waiting/unavailable/unsupported
      outcomes and last successful selected exchange. Commit file routing and status
      in separate focused slices if they are independently reviewable.

```bash
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=personal_file
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=unrelated_account_cannot_request_sibling_history_or_retained_attachment_over_tcp
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=send_file_to_account
```

## Work package 5: typed private records and private read positions

**Primary files:** new core `node/private_records.rs`, `private_records_wire.rs`,
`private_read_state.rs`; `node.rs`, `runtime.rs`, `account_sync.rs`, history import
owners; host `chat_commands/sync.rs` and command registration; frontend
`lib/api/chat.ts`, `lib/privateReadState.ts`, unread derivation owners.

- [ ] Implement the canonical signed record envelope from the design in a new
      encrypted log with independent magic/schema. Enforce account/certificate,
      signature, payload/key bounds, author sequence uniqueness and checked logical
      clock arithmetic. Persist counter allocation with append/replay so crashes do
      not reuse an author sequence for different content.
- [ ] Keep existing appearance/contact stores on V1 until explicit category
      migration. Test import-boundary persistence and rejection of late V1 writes
      before offering any cutover; report unsupported categories on old devices.
- [ ] Extend negotiated account reconciliation with collection-specific selection,
      progress and snapshot/digest binding. Signatures survive forwarding through a
      sibling; replay is idempotent. Do not introduce new global channel event kinds.
- [ ] Add private read frontiers using verified original author positions or
      explicit IDs when needed. Keep unread counts derived locally; merge acknowledgements
      only for events actually viewed while focused. Preserve a separate mark-unread
      intent. Never turn private read sync into an external read-receipt opt-in.
- [ ] Exercise duplicate/reordered/dropped delivery, two offline writers, clock
      rollback, forged author, equal sequence with different payload, policy off/on,
      crash after durable append, and a new record inserted before an active page
      cursor. Assert stable convergence and no false completed category.
- [ ] Expose a new off-by-default read-position category in typed IPC/settings.
      Verify cross-owner cancellation through existing runtime admission guards.
      Commit `feat(sync): replicate opted-in private read state` once core and host/UI
      slices each pass their focused tests.

```bash
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=private_records
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=private_read_state
cd frontend
npm test -- src/lib/unreadStorage.test.ts src/lib/privateReadState.test.ts
```

## Work package 6: portable preferences and conflict-preserving drafts

**Primary files:** new core `node/private_drafts.rs`, existing preference/record
owners; frontend new `lib/privateDrafts.ts`, composer draft owner, linked preference
publisher, `settings/model.ts`, `settings/useSettingsController.ts`, category UI.

- [ ] Publish only reviewed portable keys, with explicit category schema and size
      constraints. Keep local override/sync policy, privacy grants and machine-bound
      settings outside the replicated namespace. Add negative tests for every excluded
      settings class listed in the design.
- [ ] For drafts, carry base versions and preserve concurrent values. Define
      `resolve_draft(key, observed_versions, replacement)` as a new operation; display
      other versions until an explicit resolution covers them. Clear-after-send must
      reference the sent draft version so it cannot erase another device's later edit.
- [ ] Test concurrent offline edits, independent conversations, stale-device clear,
      tombstone replay, explicit restore, quota overflow and account switch while
      composing. Quota errors preserve current text rather than truncate it.
- [ ] Explain sync off, re-enable/backfill, remove-local and account-wide delete as
      separate actions. Confirm destructive actions and preserve existing clear/recall
      semantics. Add drafts as a new off-by-default category.
- [ ] Commit preferences and drafts as separate slices with corresponding core
      merge and frontend conflict UI tests. Evaluate Automerge/Yjs only if a later
      feature requires concurrent text editing beyond conflict-preserved drafts.

```bash
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=private_drafts
cd frontend
npm test -- src/lib/privateDrafts.test.ts src/features/chat/settings/model.test.ts src/features/chat/settings/useSettingsController.test.ts
```

## Work package 7: extended offline custody and safe compaction

**Primary files:** `node/account_sync.rs`, private record wire/store owners,
`node/{opaque_relay.rs,postbox.rs,delivery_store.rs}` and corresponding post-office
protocol/store modules located through existing owner references; new integration
target `crates/mesh-talk-core/tests/account_private_custody.rs`.

- [ ] Establish which private-record kinds are unsupported by existing opaque
      mailboxes. Add a negotiated recipient-bound encrypted envelope only for those
      kinds, reusing existing custody framing/admission, quotas and relay infrastructure.
      Relays must not get account plaintext or membership powers.
- [ ] Implement bounded retry, independent per-recipient acceptance and explicit
      expiry/storage-full reporting. A relay receipt cannot satisfy a device's import
      obligation. New-device bootstrap still requires an authorized history source.
- [ ] Test A uploads and goes offline before B connects, relay expiry, relay loss,
      duplicate custody copies, mixed relay versions and invalid recipient binding.
      State the tested retention interval/topology in deployment docs.
- [ ] Introduce authenticated collection snapshots only if compaction is necessary.
      Preserve tombstones, declare a frontier/epoch, force stale replicas to rebootstrap,
      then test that an old backup cannot resurrect deleted objects. Do not compact
      away the only deletion evidence before that boundary is enforced.
- [ ] Commit custody and compaction independently; run the focused integration
      target for changes, and actual multi-process offline scenarios at the final gate.

```bash
make test-focused TEST_PACKAGE=mesh-talk-core TEST_TARGET=account_private_custody
```

## Work package 8: separately reviewed account authority V2

**Primary files:** `identity/account.rs`, account keystore/recovery owners,
`node/{account_devices.rs,linking.rs,privacy_runtime.rs,session.rs,session_pool.rs}`,
sender-key/key-release owners, host linking/recovery commands and device settings.

- [ ] Before implementation, approve the security threat model and migration
      boundary from the design. Write canonical authority certificate/roster-chain
      formats, recovery behavior, concurrent branch handling and revoked-author event
      rules as a separate security specification with test vectors.
- [ ] Stop exporting root/management private authority to ordinary V2 devices;
      provision scoped device credentials and fresh sessions. Keep recovery material
      separately protected. Verify that enrollment history does not include live
      ratchet-state clones.
- [ ] Enforce monotonic authority epochs, durable rollback protection and full
      device signing/DH bindings at every admission/key-release boundary. Revoke jobs,
      terminate admitted sessions and rotate affected group/collection secrets.
- [ ] Make pre-compromise V1 root exposure explicit: already compromised accounts
      need a new verified trust anchor or externally pinned management authority.
      Legacy peers must not silently accept downgraded certificates for migrated
      identities. Preserve V1 rekey as the existing recovery route.
- [ ] Test forged enrollment by an ordinary device, stolen V1 root, replayed roster,
      conflicting management branches, offline stale sender, revoked-author backlog,
      key rotation, restored backup and complete device loss with/without recovery.
      Require independent crypto/protocol review before claiming secure same-ID removal.
- [ ] Only then add an accurate device revoke/recovery UI and migration rollout.
      Keep this package outside the compatible personal-sync release.

## Work package 9: compatibility, documentation and release acceptance

- [ ] Maintain golden legacy bincode fixtures for policies, preferences and
      account frames. Test legacy/upgraded pairs both directions; new capabilities
      must be negotiated before payload transfer, and unknown categories must remain
      visibly unsupported.
- [ ] Add these acceptance scenarios to
      `docs/evals/linked-device-sync-ai-eval-suite.json`: one device offline personal
      save; two-device self sync; group-only sharing; three-device combined sync with
      Bob excluded from Alice-private state; category toggles in-flight; newly linked
      sibling awaiting owner invitation; self files without blob download; restart
      deduplication; local erase versus global tombstone; legacy fallback; unavailable
      historical source; account transition with pending responses.
- [ ] For every implemented package, update current docs and mark this proposal's
      corresponding behavior with a link to implementation/evidence. Do not replace
      current architecture claims with speculative future authority/custody behavior.
- [ ] During development run only affected tests. Before PR/review, run `make test`
      and `./scripts/check-health.sh`, then committed-candidate preflight through the
      required Linux Docker and Windows SSH environments. Missing environments or
      empty Rust selections fail the gate; never report them as skipped success.
- [ ] Create a PR only when requested, through `./scripts/create-pr.sh`, with
      relevant frontend specs and explicit core/host selectors. Include visual
      regression when theme layout/snapshots are affected. Cite exact commands/results,
      tested versions/topologies and unverified physical-network limitations.

Example selector shape for the personal-message milestone, after its specs and
test filters exist:

```bash
./scripts/pr-preflight.sh --core-filter personal_message --core-filter account_sync --host-filter sync_commands_read_and_update_the_managed_node e2e/personal-messages.spec.ts
```

The supplied host filter is an existing test; add selectors for new host behavior
as implemented. Preview/adjust selectors according to
[testing feedback loop](testing-feedback-loop.md); an example is not evidence
that any environment or scenario has passed.

## Review checkpoint

The first implementation decision is whether to accept the compatible rollout:
existing category defaults, transfer/storage exclusions, preserved historical
access and deletion scope, no central service, and current identity-changing rekey.
If stronger device isolation or guaranteed non-overlapping offline availability
is required for the first release, move packages 7/8 into that release explicitly;
do not hide their protocol and trust changes inside personal-chat UI work.
