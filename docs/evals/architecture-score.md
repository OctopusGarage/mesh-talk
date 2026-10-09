# Architecture score — 2026-10-09

This is a qualitative review of the current `refactor/architecture-deepening` branch using the same rubric applied before the refactors. It measures module design in the four reviewed hot paths; it is not a repository-wide static-analysis metric.

| Dimension | Before | After | Evidence |
|---|---:|---:|---|
| Module depth | 18/25 | 23/25 | Conversation transitions, file delivery scheduling, viewport movement, and owner admission now sit behind focused interfaces. |
| Seam integrity | 17/25 | 22/25 | The state transition, delivery cursor, viewport, and host authorization rules each have one owner. |
| Test surface | 21/25 | 23/25 | Store, file delivery, rendered viewport, and registered IPC tests cover the changed seams, including restart and cancellation. |
| Locality | 14/25 | 22/25 | Each reviewed behavior can be changed at its owner rather than by editing multiple coordinating callers. |
| **Total** | **70/100** | **90/100** | Fixed equal-weight rubric; reviewer judgment, medium confidence. |

The four rounds were:

1. `store/conversationState.ts` took cache eviction, deletion protection, and conversation state transitions from `chat.ts` and `outgoingIntent.ts`.
2. `FileDeliveryScheduler` took the file worker's pending and active cursors, bounded transfer cache, admission, and exact retirement path.
3. `useConversationViewport.ts` took scroll restoration, search jumps, and follow-latest behavior from `ConversationView.tsx`.
4. `OwnerAdmission` took owner capture, checked reads, guarded local acceptance, and cancellation-safe detached enqueue lifetime for owner-sensitive IPC.

Verification: `make test` passed; `./scripts/check-health.sh` passed its complete gate. The health run included 264 Vitest tests, 210 passing Playwright tests with one skipped, 713 core library tests, 90 Tauri library tests, strict workspace Clippy, audits, and builds. The targeted rendered viewport tests for restoration, jump to latest, and search navigation also passed separately.

The score has limits. File-card assembly and durable validation still span `files.rs` and `delivery_store.rs`. Host startup ticketing remains separate from IPC admission because startup and a running operation have different lifetimes. These are the clearest candidates for a later review; this score does not claim those paths are fully localized.
