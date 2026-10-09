# Architecture score — 2026-10-09

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
