# Channel lifecycle worktree evaluation — 2026-10-13

## Scope

Uncommitted channel invitation, acceptance, ownership transfer, departure,
hide/restore, activity, Invisible-mode routing, Tauri IPC, and React UI changes.
The evaluation used real Rust nodes, Playwright's mocked Tauri browser harness,
the model-backed agent suite, and read-only model review of the worktree diff.

## Evidence

| Gate | Result |
| --- | --- |
| `make e2e` | Five real multi-process DM/channel/file/history/relay cases and the large-directory case passed before the review fixes. |
| `cargo test -p mesh-talk-core --test channel_and_file_cli -- --ignored --test-threads=1` | 2/2 real CLI channel and file cases passed after the fixes. |
| `make test` | Passed after the fixes; core library: 759 passed, 1 ignored. Includes loopback invite, transfer, leave, and privacy routing assertions. |
| Full `frontend/npm run e2e` | 275 passed, 3 skipped, 8 failed on the first parallel run. Seven failed cases passed on single-worker rerun; the remaining failure was an expected members-dialog screenshot change. Its baseline was inspected, updated, and passed. |
| Focused Playwright rerun | 21/21 channel lifecycle, member dialog, hidden-contact, and privacy cases passed with one worker after the UI changes. |
| Frontend units | 331/331 passed with one worker. The first parallel run had one unrelated 5-second pack-check timeout; its focused rerun passed 16/16. |
| Lint/build | Rust fmt, workspace Clippy (`--all-targets -- -D warnings`), ESLint, TypeScript, Prettier, and frontend build passed. |
| AI eval | `make eval-smoke` passed; real model-backed `make ai-eval` passed 5/5 agent-contract cases. |

Read-only model reviews found reproducible channel races and stale UI state.
Regression tests were added before fixing the concurrent leave/snapshot merge,
late-invite replay, ownership-transfer success check, legacy auto-join, Invisible-mode
sync/admission projections, and active member refresh. The final security review
also identified the bootstrap privacy boundary below.

## Invisible-mode ancestry boundary follow-up

The follow-up change stages an unknown channel's ancestry in the authenticated
sync connection rather than writing it to the encrypted event log. The receiver
validates event signatures and causal order in an ephemeral log, then verifies
the signed invitation through the channel membership projection. Authorized
ancestors and the invitation land together through an atomic file replacement.
Uninvited or unauthorized histories disappear when the connection ends; staged
ids can advance reconciliation but do not count as durable custody.

The staging area is capped at 8 MiB per connection. A late invitation behind
more ancestry is rejected rather than allowing unbounded memory use or early
persistence. Supporting older, larger channels would require a separate compact
proof in the sync protocol. This is a deliberate availability limit.

Follow-up verification on the worktree: `cargo test --workspace --quiet` passed
(core library 763 passed, 1 ignored), workspace Clippy with `--all-targets`
passed, and `make e2e` passed five real multi-process cases plus the large
directory case. `make eval-smoke` and model-backed `make ai-eval` passed (5/5).
Focused tests cover pre-invite non-persistence, forged owner rejection,
owner-transfer invitations, trusted post-office carriage, and removal cutoff.
A read-only model review found the post-office and removal issues, which were
reproduced and fixed. Atomic batch tests inspect the old on-disk log immediately
before rename, then reopen the complete replacement. As in other record-log
rewrites, a parent-directory sync failure after successful rename is logged;
the replacement is atomic, while power-loss durability of that rename is best
effort in this exceptional case.

The browser E2E harness mocks Tauri IPC. Its results do not establish native
desktop interaction or Invisible-mode behavior across real machines. The first
full browser run was reconciled by isolated reruns and the focused suite; a
single fresh full-suite green run was not performed after the screenshot update.
