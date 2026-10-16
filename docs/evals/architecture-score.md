# Architecture health assessments

## Current architecture-health loop — 2026-10-16

The current loop starts at **85/100** at `cc303ec`. It uses five equally weighted
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
