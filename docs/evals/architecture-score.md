# Architecture health assessments

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

| Dimension | Starting | Round 1 | Round 2 | Round 3 | Evidence |
|---|---:|---:|---:|---:|---|
| Module depth | 22/25 | 23/25 | 23/25 | 23/25 | The Account page module owns bounded record assembly and scanning; bridge response assembly belongs to the directory module. |
| Seam integrity | 23/25 | 23/25 | 23/25 | 23/25 | Core, host, and client interfaces are unchanged; wire, storage, and settings persistence seams remain distinct. |
| Test surface | 24/25 | 24/25 | 24/25 | 24/25 | Focused page, bridge, and settings races plus linked-device, bridge process, and UI E2E coverage; full-round bridge refresh still lacks one isolated test interface. |
| Locality | 21/25 | 22/25 | 23/25 | 24/25 | Account record completion, cross-page gateway consistency, and Settings dialog save/load lifetime now each have one owner. |
| **Total** | **90/100** | **92/100** | **93/100** | **94/100** | Same equal-weight rubric; reviewer judgment with medium confidence. |

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

## Earlier review — 2026-10-09

**95/100**, up from **86/100** in the broad-scope review of commit `6640d30`.
This is a qualitative score for the codebase's active core, host, and frontend
seams using a fixed four-part rubric. It is reviewer judgment with medium
confidence, not a repository-wide static-analysis metric. The earlier 90/100
in this document covered only four selected refactors; the 86/100 baseline
reassessed the wider hot paths before the three rounds below.

| Dimension | Broad baseline | Current | Evidence |
|---|---:|---:|---|
| Module depth | 22/25 | 24/25 | `NodeState` owns host lifecycle tickets and retirement; `DeliveryStore` prepares the outgoing manifest transaction and derives File cards; conversation state settles sends behind one transition interface. |
| Seam integrity | 21/25 | 24/25 | Host commands delegate lifecycle changes to `NodeState`; send/receive file paths share store-owned card construction; frontend send orchestration delegates cache, deletion, and intent completion rules to `conversationState.ts`. |
| Test surface | 23/25 | 24/25 | Existing registered-command and Node runtime tests exercise the host and file interfaces; new public store-action tests cover late accepted sends after cache eviction and authoritative file metadata arriving before acceptance. |
| Locality | 20/25 | 23/25 | Lifecycle ordering, card identity, and exact-ID completion each have one owner, leaving callers to coordinate IPC or network work. |
| **Total** | **86/100** | **95/100** | Equal weights; same rubric in each round. |

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
