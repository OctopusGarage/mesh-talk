# Architecture health assessments

## Feature reuse follow-up — 2026-10-19

The [45-feature responsibility review](feature-responsibility-review-2026-10-19.md#implementation-follow-up)
improved from **92 to 100/100 under its balanced Brooks finding rubric** after
resolving channel-action coordination, attachment approval duplication, shared
highlighting and motion/sticker ownership. Independent spec and code-quality
reviews found no actionable gaps; focused validation passed 380 frontend tests
and 44 workflow-tooling tests. This meets the requested 95 threshold under that
rubric. It is not a fresh exhaustive audit or a numerical reassessment of the
separate five-dimension result below, which remains 95/100.

## Current architecture-health loop — 2026-10-19

**Current maintained result: 95/100 after three iterations. The architecture goal is achieved; release/PR gates remain separate.**

Baseline: `491adf4` on `fix/architecture-audit-20261019`, rated **87/100** using
the explicitly requested improve-codebase-architecture skill. That skill supplies
no numerical formula. This loop uses five equally weighted dimensions out of 20;
these are qualitative engineering judgments, not coverage percentages or points
awarded for moving files. The earlier 100/100 in conversation measured closure
of enumerated findings and is not this structural rubric.

| Dimension                                    |   Baseline | Iteration 1 | Iteration 2 | Iteration 3 |
| -------------------------------------------- | ---------: | ----------: | ----------: | ----------: |
| Responsibility clarity                       |         18 |          19 |          19 |          19 |
| Module depth and interface simplicity        |         17 |          18 |          19 |          19 |
| Reuse and locality                           |         17 |          19 |          19 |          19 |
| Test seams                                   |         18 |          19 |          19 |          19 |
| AI navigability and parallel change locality |         17 |          18 |          18 |          19 |
| **Total**                                    | **87/100** |  **93/100** |  **94/100** |  **95/100** |

Per-dimension anchors remain fixed: 20 clear ownership with behavior hidden behind
useful interfaces, 15 sound structure with recurring coordination friction,
10 substantial change propagation, 5 pervasive coupling, 0 no coherent ownership.
Confidence is moderate-high for inspected modules and moderate repository-wide.
The review samples all four Rust crate roles and frontend ownership, concentrating
on hot spots in the last 80 non-merge commits; it is not an exhaustive line audit
or a security, performance, release or platform certification.

### Iteration 1: concentrate three existing decisions

- Conversation history now owns reload, bounded revision scans, expanded archive
  coverage and coordinated reception updates. Outgoing intents retain acceptance
  and retry. The chat/outgoing runtime import cycle is removed; reception owns the
  unread projection used by live Events and history.
- AccountProfiles owns avatar/language journals and current signed publication,
  including shared reconstruction, revisions, legacy merging, failure ordering
  and recovery snapshots/imports. Node retains authenticated Event handling,
  authorization and delivery. Immutable receive decisions retain their semantics.
- Desktop session owns one live user/token/password and exact-session generation.
  Authentication persists/verifies credentials and commits to that same owner;
  commands no longer synchronize duplicate user/token stores. Runtime startup and
  retirement remain host responsibilities, with compatibility exports retained.

Independent reviews found no actionable issues. Independent scoring retained
93/100 because history still depended on aggregate ChatState and tests required
initializing the entire chat store.

### Iteration 2: history defines its state interface

ConversationHistoryState and its runtime context are defined by the history owner;
chat composes them. Setters cannot patch session epochs or capabilities. Shared
Conversation data lives in conversationModel.ts, with existing exports preserved.
Cache/history defaults are reused by production and standalone history tests,
which no longer import the chat store. Identity invalidation preserves its prior
revision and request counters. Independent review found no issues; scoring rose
to 94/100, retaining the outgoing-intent aggregate getter/setter deduction.

### Iteration 3: outgoing intents define their own interface

OutgoingIntentState and its context replace the aggregate chat getter/setter.
The context supplies only runtime validity, active Conversation, sender identity,
reload, status refresh and error reporting. Chat composes the history and outgoing
contracts. Patches cannot write unrelated chat fields; error reporting reuses the
existing capability under the captured lease. Standalone tests exercise real
acceptance, hydration failure, stale-owner completion, file failure/cleanup,
capacity refusal and status-poll recursion protection without initializing chat.

Independent review found no issues. The assessor raised AI navigability/locality
from 18 to 19 because both history and outgoing implementation/tests now depend on
owner-defined contracts, not aggregate composition. The final score is **95/100**.

Remaining deductions are intentional: history/outgoing contexts still expose
shared mutable state and reload/status ordering; authentication/transfers remain
environmental dependencies; core sibling modules still access broad crate-visible
Node state; host auth/runtime/commit lifetimes need coordinated changes; trusted
integration callbacks still receive a full Node. These are acceptable residuals
in the inspected evidence, not a claim of architectural perfection. Further
extraction should follow demonstrated change friction rather than score chasing.

### Validation and limits

Executed focused selections:

```sh
cd frontend
npm test -- src/store/conversationHistory.test.ts src/store/chat.test.ts src/store/reception.test.ts src/store/outgoingIntent.test.ts src/lib/moduleBoundaries.test.ts
npm run lint
npm run typecheck
npm run build
cd ..
cargo test -p mesh-talk-core --lib --features fast-test-kdf -- profile language recovery avatar
cargo test -p mesh-talk --lib services::auth_service::tests
cargo test -p mesh-talk --lib -- desktop_session:: runtime_admission:: commands::tests:: owner_commands::tests::
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

The final implementation passed **158 frontend, 75 core and 74 host tests**
(**307 distinct selected tests**). The built core/host test executables were also
rerun with these selections.
The core selection includes real avatar/language loopback, restart, legacy sibling
and recovery checks. Frontend lint/type/build, Rust formatting, strict workspace
Clippy and diff checks passed. Clippy caught one redundant return binding in a
modified fixture; it was corrected before the successful rerun.

A TypeScript-AST scan of **171 production frontend modules** found no static runtime
import cycles; baseline had one cycle across 169 modules. Type-only imports are
excluded; dynamic loading and native/plugin behavior are outside that graph.
The existing Vite main-chunk size advisory remains. No comprehensive health gate,
full test suite, E2E or physical-machine validation was run for this loop.

---

## Earlier architecture-health loop — 2026-10-16

**Historical result: 95/100 (Round 3). That architecture goal was achieved;
release/PR gates are separate and must pass before merge.**

That loop started at **85/100** at `cc303ec`. It uses five equally weighted
architecture dimensions for the whole repository's structure, with deeper
inspection of recent lifecycle, UI, networking and delivery changes. The goal is
an evidence-supported **95/100** under this same rubric. Earlier assessments
below use different scopes or rules and are historical context, not a numerical
time series.

| Dimension                           |   Baseline | Evidence at baseline                                                                                     |
| ----------------------------------- | ---------: | -------------------------------------------------------------------------------------------------------- |
| Module depth                        |      17/20 | Deep storage, sync and settings modules; intake and acceptance callers still carry lifecycle sequencing. |
| Locality and reuse                  |      16/20 | Shared roster/relay policy; independent CLI/runtime lifecycle and host admission implementations.        |
| Dependency direction and ownership  |      18/20 | UI-free core and enforced IPC adapter direction; authority contracts vary across host commands.          |
| Interface-level testability         |      17/20 | Real persistence/loopback/retirement tests; sparse intake and diagnostic lifecycle characterization.     |
| Maintainability and AI navigability |      17/20 | Domain and ownership guides; conversation and diagnostic rendering mix independent responsibilities.     |
| **Total**                           | **85/100** | Reviewer judgment with medium confidence.                                                                |

Per-dimension anchors remain fixed: 0 absent, 5 fragmented, 10 mixed, 15 strong
with notable friction, 20 consistently strong in the inspected evidence. Scores
between anchors are qualitative judgments, not coverage percentages or points
automatically earned for moving files. This review does not certify security,
performance, remote CI status or native-platform compatibility.

### Round 1: lifecycle ownership and runtime admission — 92/100

| Dimension                           |   Baseline |    Round 1 | Evidence and remaining deduction                                                                                                                                                      |
| ----------------------------------- | ---------: | ---------: | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Module depth                        |      17/20 |      19/20 | Diagnostic and attachment modules absorb lifecycle obligations from their rendering callers; outgoing acceptance still spans several owners.                                          |
| Locality and reuse                  |      16/20 |      18/20 | Intake sources share staging/lease rules; runtime/session validation has one implementation. CLI/desktop task assembly and asynchronous admission still diverge.                      |
| Dependency direction and ownership  |      18/20 |      18/20 | Shared synchronous admission is proven, but unconverted asynchronous command families retain different authority contracts.                                                           |
| Interface-level testability         |      17/20 |      18/20 | Mounted intake/diagnostic and registered IPC regressions establish the corrected lifetimes; asynchronous host seams and some cancellation behavior remain less characterized.         |
| Maintainability and AI navigability |      17/20 |      19/20 | Rendering delegates acquisition/observation lifetimes to feature-local owners; host admission and snapshot limits are documented. Participant assembly remains a shared-edit hotspot. |
| **Total**                           | **85/100** | **92/100** | Same five-dimension rubric; medium confidence; goal remains active.                                                                                                                   |

An independent read-only reassessment proposed 93/100. The maintained ledger
conservatively retains dependency direction/ownership at 18, since the baseline's
uneven asynchronous authority contracts remain open. No score increase is
credited merely for defining a new module or passing an unrelated suite.

- `8a2293f` concentrates diagnostic refreshes, serial polling, bounded activity
  history and native action continuations in a feature-local controller. A
  failing mounted regression demonstrated late Finder reveal after closure;
  the controller suppresses follow-up native effects and stale publication.
  Another regression covers feedback expiry while a later rescan is pending.
- `082318c` moves existing owner-command admission into `runtime_admission.rs`
  and reuses it for all five linked-device sync/preference commands. A failing
  registered-command test demonstrated a mismatched session changing the old
  runtime's sync policy. The shared module captures before the runtime wait
  and holds matching session authority through synchronous operations. Queued
  same-owner replacement, logout and app-lock rejection preserve persisted
  policy/preference bytes. Existing delivery cancellation/commit checks remain.

- `0453dcc` concentrates picker/drop/paste/screenshot acquisition in
  `attachments/useAttachmentIntake.ts`. Deferred staging now removes abandoned
  temporary files; screenshot edits retain their origin. Composer consumes
  explicit handoff progress, preserving unsent and concurrently pasted files.
  Review caught and corrected abandoned-batch resend suppression. Retired native
  listeners ignore late callbacks and consume registration failure.
- `ea1c381` captures the entry session lease before an owner-scoped runtime
  lookup waits. A failing regression demonstrated queued work adopting a new
  generation of the same owner. Both admission and runtime lookup now reuse
  one session/installation check. The test uses the production installation
  interface and verifies queued and fresh lookups reject a stale installation.

Verification executed for these commits:

```sh
cd frontend
npm test -- src/features/chat/attachments src/features/chat/ConversationView.test.ts src/features/chat/clipboardFiles.test.ts src/features/chat/diagnostics src/features/chat/DiagnosticsDialog.test.ts src/features/chat/useSerialPoll.test.ts src/lib/moduleBoundaries.test.ts src/lib/rosterProjection.test.ts src/features/chat/settings/model.test.ts
npm run typecheck
npm run build
cd ..
cargo test -p mesh-talk --lib owner_commands::tests -- --nocapture
cargo test -p mesh-talk --lib chat_commands::sync::tests
cargo test -p mesh-talk --lib owner_scoped_node_handle -- --nocapture
cargo clippy -p mesh-talk --all-targets -- -D warnings
```

The combined focused frontend slice passed **67 tests across 9 files**; host
slices passed **12 + 1 + 2 tests**. Changed frontend files passed ESLint and
Prettier, including the maintained documentation; Rust formatting and strict
host Clippy passed. The production build retains its existing >700 kB main-chunk
warning. Specification and code-quality reviews passed for all slices. The
static frontend inventory is 149 production modules, 568 runtime edges and 66
type-only edges, with no detected static runtime cycle. Dynamic imports and
native/plugin behavior are outside that graph. No full-suite, full E2E or
physical-machine gate ran.

Host scope remains limited: unconverted legacy asynchronous commands, privacy
and settings need their actual commit seams characterized. A shared check around
an arbitrary future cannot prevent that future's side effects. CLI/desktop
participant lifecycle duplication and outgoing acceptance sequencing remain
open. A resolved file-send promise still does not expose intent-cap refusal;
store ownership and durable recipient confirmation remain separate facts.
The existing early-cancellation `Promise<void>` behavior also needs a real
Composer scenario for same-conversation runtime invalidation before claiming
that every canceled batch retains its unsent files.

Next: shared owned participant lifecycle, preserving host-specific bootstrap and
retirement guarantees; then characterize remaining asynchronous host commit
seams and conditional delivery acceptance consolidation. The goal stays active
until the fixed rubric supports at least 95/100.

### Round 2: shared participant ownership and legacy account acceptance — 94/100

| Dimension                           |    Round 1 |    Round 2 | Evidence and remaining deduction                                                                                                                                                 |
| ----------------------------------- | ---------: | ---------: | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Module depth                        |      19/20 |      19/20 | The participant owner absorbs common producer assembly and retirement from desktop and CLI; acceptance still coordinates distinct crypto, transaction and authority owners.      |
| Locality and reuse                  |      18/20 |      19/20 | One network lifetime owns discovery, accept, private routes, mailbox drain, bridge refresh and file pulls. Remaining host command families retain separate authority sequencing. |
| Dependency direction and ownership  |      18/20 |      18/20 | Legacy account text/sticker/file sends now use the actual guarded grant/WAL seam. Device/channel sends, privacy and settings need further authority characterization.            |
| Interface-level testability         |      18/20 |      19/20 | Public enqueue cancellation, real registered IPC and CLI process tests cover producer lifetimes and session changes; remaining asynchronous seams remain open.                   |
| Maintainability and AI navigability |      19/20 |      19/20 | Shared participant ownership and host-specific responsibilities are documented; large delivery/channel paths still require tracing distinct owners.                              |
| **Total**                           | **92/100** | **94/100** | Independent specification and quality reviews passed; same fixed rubric, medium confidence. Goal remains active.                                                                 |

`PreparedParticipant` binds resources without launching work; desktop launch stays
inside its authorization callback. `ParticipantRuntime` owns common producers and
adopted host tasks. Explicit stop closes work admission, aborts/joins tasks and
drains blocking work. Drop requests cancellation only. CLI profile paths, fixed
ports, REPL behavior and post-office mode remain host policy.

A specification review found that adopted CLI file sends could leave blocking
staging running after stop returned. A public enqueue regression reproduced this
failure. Staging now holds its retirement permit inside the actual writer, as do
CLI autosaves. Retirement waits even after its async caller is canceled and
subsequent staging admission is rejected.

Legacy account text/sticker/file sends capture the entry session and reuse
`RuntimeAdmission` at local grants and durable acceptance. Failing registered IPC
regressions demonstrated mismatched-session text and file sends succeeding before
the fix. Tests also cover same-owner replacement, real logout during a privacy
grant wait, original results/history and preservation of ordinary source files.
Immediate best-effort flush behavior and persisted/wire formats are unchanged.
Legacy payloads still cannot identify an intent created before IPC entry.

Parent verification:

```sh
cargo test -p mesh-talk --lib owner_commands::tests -- --nocapture
cargo test -p mesh-talk-core --lib participant_runtime_tests -- --nocapture
cargo test -p mesh-talk-core --lib node::runtime -- --nocapture
cargo test -p mesh-talk-core --test two_node_cli participant_cli -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

These passed **17 host + 4 participant + 31 runtime + 2 CLI process tests**.
The implementation worker also passed 25 CLI binary tests and focused routed-DM
and file process assertions. Documentation formatting and diff checks passed.
No full gate or remote CI claim is made at this checkpoint.

No extra delivery sequencing abstraction was added: `DeliveryStore` and
`accept_outgoing_transaction` already own durable begin/recovery. DM sealing,
file staging/quota and host authorization have distinct obligations; moving them
behind a forwarding helper alone would not deepen the module. Next, characterize
and consolidate the remaining privacy authority seam before reassessment.

### Round 3: privacy commit authority — 95/100

| Dimension                           |    Round 2 |    Round 3 | Evidence and remaining deduction                                                                                                                                                                       |
| ----------------------------------- | ---------: | ---------: | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Module depth                        |      19/20 |      19/20 | Privacy waits and synchronous commits share one implementation; live settings still leaves callers coordinating later effects.                                                                         |
| Locality and reuse                  |      19/20 |      19/20 | Privacy now reuses host admission rather than its own generation recipe; device/channel and live settings coordination remains separate.                                                               |
| Dependency direction and ownership  |      18/20 |      19/20 | Actual privacy writes and invalidation are enclosed by session/installation authority; canceled IPC retains lifecycle ownership. Remaining device/channel/settings contracts prevent full consistency. |
| Interface-level testability         |      19/20 |      19/20 | Registered IPC, persisted-byte, callback-scope and cancellation assertions cover corrected privacy lifetimes. Other asynchronous families remain less characterized.                                   |
| Maintainability and AI navigability |      19/20 |      19/20 | Shared admission and documented lock ordering make privacy authority traceable; heterogeneous legacy paths remain.                                                                                     |
| **Total**                           | **94/100** | **95/100** | Independent specification and quality reviews passed; same fixed rubric, medium confidence. Architecture goal achieved.                                                                                |

The failing baseline allowed registered `get_privacy` while the app was locked.
Privacy commands now capture shared admission before waiting and check observations
and responses against the current session and runtime installation. Detached
execution keeps the lifecycle barrier even if the IPC waiter is canceled.

Core `set_invisible_guarded`, `set_allowed_guarded` and
`initiate_contact_guarded` enter host authority after the async privacy gate and
outside synchronous privacy-state locks. The supplied commit encloses actual
policy/proof/route writes, generation updates and synchronous bridge invalidation.
Network announcement drain/restore and private presence publication remain awaits
outside session authority. Legacy `*_if` APIs delegate while retaining validation
ordering, public formats and existing behavior. The commit callback contract
requires exactly one invocation under authority and propagation of its result.

Verification:

```sh
cargo test -p mesh-talk-core --lib node::privacy_runtime::tests --features fast-test-kdf -- --nocapture
cargo test -p mesh-talk --lib owner_commands::tests -- --nocapture
cargo test -p mesh-talk --lib privacy_commands::tests -- --nocapture
cargo test -p mesh-talk --lib privacy -- --nocapture
cargo test -p mesh-talk-core --lib guarded_privacy_commits -- --nocapture
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The implementation checks passed **43 core + 20 owner + 2 privacy IPC tests**;
independent parent checks passed **7 affected host tests + 1 core commit-scope
test**. Real registered scenarios reject app lock, stale installation, queued
same-owner replacement and logout during all three policy-gate waits without
changing persisted privacy/proof/route bytes. Callback-scope coverage checks that
writes/generation changes occur before authority exits and that authority is
entered without holding a privacy-state lock. Cancellation coverage verifies
that retirement cannot pass a still-running privacy operation.

The independent reviewer supports raising ownership from 18 to 19 for removing
this concrete check/commit and cancellation gap. No points are awarded merely
for moving files or adding methods. The other four dimensions stay at 19, with
explicit residual deductions. No extra delivery abstraction is justified by size
alone: existing WAL/recovery ownership remains intact.

**Stop condition:** the optimization loop ends at this checkpoint. The score is
not a claim that all bugs are eliminated or that security, performance, native
compatibility or remote CI is certified. PR preparation and full local/remote
gates follow separately; failures in those gates must still be resolved.

## Optimization series — 2026-10-15

The repository-wide starting assessment was **90/100**: module depth 22/25,
seam integrity 23/25, test surface 24/25, and locality 21/25. This is a
qualitative review of the current core, host, and frontend, including the
recent bridge and linked-device sync work. It is not a measured decline from
the narrower 9 October review below.

Round 1 concentrates Account history page records, bounded scanning, and
inbound frame assembly in `node/account_sync/page.rs`. The receive loop now
coordinates the wire stream and applies a completed page through one method.
The Node interface, versioned frames, limits, and persistence formats are
unchanged. A focused assembler regression covers incomplete records, overflow,
and a complete page; existing linked-device backfill and process tests exercise
the path end to end.

| Dimension      |   Starting |    Round 1 |    Round 2 |    Round 3 |    Round 4 | Evidence                                                                                                                           |
| -------------- | ---------: | ---------: | ---------: | ---------: | ---------: | ---------------------------------------------------------------------------------------------------------------------------------- |
| Module depth   |      22/25 |      23/25 |      23/25 |      23/25 |      23/25 | The Account page module owns bounded record assembly and scanning; bridge response assembly belongs to the directory module.       |
| Seam integrity |      23/25 |      23/25 |      23/25 |      23/25 |      24/25 | Host settings and periodic retention use one owner-scoped `NodeState` interface rather than duplicating runtime/session checks.    |
| Test surface   |      24/25 |      24/25 |      24/25 |      24/25 |      24/25 | Focused page, bridge, settings, and host ownership regressions; full-round bridge refresh still lacks one isolated test interface. |
| Locality       |      21/25 |      22/25 |      23/25 |      24/25 |      24/25 | Account completion, gateway consistency, Settings dialog lifetime, and host account ownership each have one owner.                 |
| **Total**      | **90/100** | **92/100** | **93/100** | **94/100** | **95/100** | Same equal-weight rubric; reviewer judgment with medium confidence.                                                                |

Verification: `cargo test -p mesh-talk-core --lib node::account_sync`,
`cargo test -p mesh-talk-core --lib account_history_backfills_to_a_linked_device`,
`make test`, `cargo clippy --all-targets -- -D warnings`, and `make e2e` passed.
Round 2 moves directory response assembly from the Node's network query into
`node/bridge_directory.rs`. The directory now owns v2/v1 decoding, legacy
gateway selection, cross-page gateway consistency, and the 128-proof bound.
The Node still owns the 128-page and 10-second network limits. A regression
rejects conflicting valid gateway routes across pages. `make test`,
`cargo test -p mesh-talk-core --test bridge_cross_lan -- --ignored --test-threads=1`,
`cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check`
passed. Wire bytes and public interfaces are unchanged. The score remains
below the 95/100 goal because bridge refresh, Account sync application, and
settings ownership still span multiple places.

Round 3 gives the Settings dialog one session owner for ordered saves, load
generations, account switches, and state publication. Queued writes from a
previous account are skipped; writes already started or queued when the dialog
closes still finish for the same account, while their stale UI updates are
suppressed. Saved privacy and OS autostart read-backs now publish through the
same successful-save path. A late directory picker result cannot save into a
new view. Existing settings IPC and stored document shapes are unchanged.
The full frontend unit suite (378 tests), typecheck, lint, format check, and
production build passed. Fifteen targeted Playwright settings/privacy/dialog
tests passed; after moving owner invalidation to a layout effect, the five
settings recovery tests passed again. The remaining point to the target is
primarily the host's repeated settings application lease and the dialog's
field-level presentation state.

Round 4 moves host runtime and live-session ownership checks into
`NodeState::node_for_owner`. Settings application and periodic retention use
that interface; immediate retention pruning checks ownership again after
relay application can yield. The focused host regression covers the matching
owner, a different requested owner, and a replaced session. The normal local
workflow now selects one Rust module/integration target or frontend test file;
pre-commit performs source checks, while comprehensive suites remain the PR/CI
gate. This review rates seam integrity one point higher. The 95/100 is a
qualitative architecture score, not a measured performance or test coverage
percentage. Round 4 was validated with focused host tests, strict Clippy,
formatting, and workflow checks; the comprehensive health gate was stopped
when the user changed the local testing policy.

## Earlier review — 2026-10-09

**95/100**, up from **86/100** in the broad-scope review of commit `6640d30`.
This is a qualitative score for the codebase's active core, host, and frontend
seams using a fixed four-part rubric. It is reviewer judgment with medium
confidence, not a repository-wide static-analysis metric. The earlier 90/100
in this document covered only four selected refactors; the 86/100 baseline
reassessed the wider hot paths before the three rounds below.

| Dimension      | Broad baseline |    Current | Evidence                                                                                                                                                                                                                            |
| -------------- | -------------: | ---------: | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Module depth   |          22/25 |      24/25 | `NodeState` owns host lifecycle tickets and retirement; `DeliveryStore` prepares the outgoing manifest transaction and derives File cards; conversation state settles sends behind one transition interface.                        |
| Seam integrity |          21/25 |      24/25 | Host commands delegate lifecycle changes to `NodeState`; send/receive file paths share store-owned card construction; frontend send orchestration delegates cache, deletion, and intent completion rules to `conversationState.ts`. |
| Test surface   |          23/25 |      24/25 | Existing registered-command and Node runtime tests exercise the host and file interfaces; new public store-action tests cover late accepted sends after cache eviction and authoritative file metadata arriving before acceptance.  |
| Locality       |          20/25 |      23/25 | Lifecycle ordering, card identity, and exact-ID completion each have one owner, leaving callers to coordinate IPC or network work.                                                                                                  |
| **Total**      |     **86/100** | **95/100** | Equal weights; same rubric in each round.                                                                                                                                                                                           |

## Rounds

1. `aebed81` — Host runtime lifecycle moved from `commands.rs` into `NodeState`, next to the installed node it governs. **90/100** after this round.
2. `592234f` — The delivery store took outgoing manifest transaction preparation and both File card constructors. Signed destination events and validated staged chunks remain the inputs. **93/100** after this round.
3. `682281d` — Conversation state took exact-ID send completion, including deletion protection, metadata/status preservation, and cache eviction behavior. Send orchestration still performs IPC. Two public store-action regressions cover the newly concentrated interface. **95/100** after this round.

The deletion test favors each module: removing it would spread lifecycle,
File card, or completion rules back across its callers. No new adapter was
introduced for a hypothetical variant.

## Verification and limits

`make test` and the complete `./scripts/check-health.sh` gate passed on
`refactor/architecture-deepening`. The gate included 266 Vitest tests, 210
passing Playwright tests with one skip, 713 core library tests, 90 Tauri
library tests, strict workspace Clippy, scans, and builds. The 64 focused
delivery runtime tests and 60 pre-addition frontend store tests also passed
during the rounds. The two new store tests passed with the full 266-test suite.

User-visible behavior, Tauri command shapes, versioned wire layouts, and stored
data formats were kept stable. This is supported by the changed interface
surfaces and regression checks, not an exhaustive format compatibility proof.
The host runtime lock remains exposed to legacy command callers, and incoming
manifest validation still coordinates network proof with the store. Those
remaining seams account for the score staying below 100.
