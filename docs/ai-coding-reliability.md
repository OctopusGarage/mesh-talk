# AI coding reliability guide

Use this guide before implementing or reviewing a change, especially a bug fix,
refactor, merge resolution or optimization. Its purpose is to turn recurring
failure modes into concrete design questions and executable checks. Compiling,
adding a timeout or passing a happy-path test does not establish reliability.

This guide owns the cross-cutting review method and reusable failure patterns.
[Module ownership](module-boundaries.md) owns implementation boundaries;
[architecture](ARCHITECTURE.md), [network contracts](network-design-reference.md)
and feature guides own behavior and resource limits. The
[testing workflow](testing-feedback-loop.md) owns commands, selection and gates.
Link to those owners instead of copying their changing contracts here. When code
and a contract disagree, resolve the intended behavior rather than documenting
the regression as the new rule.

## Use on every change

1. **Locate the owner and consumers.** Read the owning implementation, callers,
   existing tests and relevant guide before adding another helper or state owner.
   Include callbacks, IPC handlers, retries, startup, shutdown and recovery paths;
   an import graph alone cannot reveal all consumers.
2. **State the invariant and select risks.** Describe what must remain true on
   success, failure, cancellation and replacement. Use the table below to select
   applicable rules. A small diff can change lock order or lifetime semantics.
3. **Trace the failure path before editing.** For concurrency work, sketch who
   owns each lock, permit, task and mutable state; what it waits for; and who
   releases it. For a reproducible bug, construct a focused failing regression.
   Record unresolved hypotheses as hypotheses, not confirmed defects.
4. **Implement at the existing boundary.** Preserve authority, durable acceptance,
   compatibility and resource budgets. Recheck invariants after moving work
   outside a guard. Evaluate the proposed fix for new races and deadlocks too.
5. **Verify the changed boundary and report evidence.** Run affected checks and
   required boundary scenarios under the testing workflow. Explain results,
   exclusions and unresolved evidence. Update this guide when the defect exposes
   a reusable failure pattern; follow the maintenance procedure below.

Keep the risk explanation proportional to the change. A documentation edit does
not require a new concurrency test. A changed async callback does require a
lifetime review even when the diff is one line. Put task-specific reasoning and
results in the PR or ignored local evidence, not a new permanent report.

## Select rules by the changed boundary

| Change or symptom                                                        | Read at least                                                                                                                                         |
| ------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Locks, callbacks, synchronous dispatch, UI freezes                       | [R01](#r01-lock-order-and-reentrant-calls), [R02](#r02-thread-and-scheduler-progress), [R10](#r10-evidence-that-exercises-the-failure)                |
| Background work, native providers, logout, shutdown or cancellation      | [R02](#r02-thread-and-scheduler-progress), [R03](#r03-actual-work-owns-its-lifetime), [R06](#r06-exact-authority-and-stale-completions)               |
| Network requests, retries, polling, queues or batches                    | [R03](#r03-actual-work-owns-its-lifetime), [R04](#r04-deadlines-cover-the-whole-operation), [R05](#r05-bounded-work-must-also-make-progress)          |
| React effects, IPC reads, dialogs, optimistic updates or mutation status | [R06](#r06-exact-authority-and-stale-completions), [R07](#r07-observation-is-not-mutation-ownership), [R10](#r10-evidence-that-exercises-the-failure) |
| Storage, settings, imports, deletion or recovery                         | [R06](#r06-exact-authority-and-stale-completions), [R08](#r08-durable-state-and-failure-semantics), [R09](#r09-bound-input-and-resource-use)          |
| IPC/API inputs, files, media or protocol changes                         | [R06](#r06-exact-authority-and-stale-completions), [R08](#r08-durable-state-and-failure-semantics), [R09](#r09-bound-input-and-resource-use)          |
| Merge resolution, shared helper or dependency changes                    | Revisit the affected callers and all applicable rules; a clean textual merge does not establish compatible ownership or semantics.                    |

These are starting points, not permission to ignore another reachable boundary.
Do not add a global lock, retry loop, timeout or shared abstraction solely to make
the local symptom disappear.

A frozen UI may result from a lock cycle, blocked event thread, starvation, lost
wakeup, unbounded work or stale loading state. Identify the mechanism and the
missing progress condition before selecting a fix; these need different evidence.

## R01: Lock order and reentrant calls

**Failure pattern:** one path takes A then B while another takes B then A; or a
helper/callback reacquires a nonrecursive lock its caller already holds. The
second case can deadlock on a single thread. Nested reads also need review when
a queued writer can prevent a new read from completing.

Trace actual acquisition order through callees, authorization callbacks,
destructors and error cleanup. Include main-thread dispatch and task joins as
wait dependencies, not just mutexes. A typical UI cycle is:

```text
background task: holds session guard -> waits for main-thread callback
main thread:     handles UI event    -> waits for session guard
```

Use the owning module's order; do not invent one universal order for unrelated
subsystems. Make whether a helper acquires a guard or requires one explicit.
Avoid invoking arbitrary callbacks while guarded. Where a commit callback must
run inside an authority guard, keep that contract narrow and inspect its complete
call chain. Queue presentation without retaining a guard needed by the UI;
validate captured authority when the queued callback executes.

**Evidence:** exercise both competing paths and the real authorization variant.
An ordinary admission fixture does not cover a sensitive proof that itself locks
app state. Assert that the other operation progresses, not just that the first
eventually returns. See `checked_at_lock_revision` in
[runtime admission](../src-tauri/src/runtime_admission.rs), the verified-login
regression in [shared login tests](../src-tauri/src/shared_account_login/tests.rs),
and queued notification tests in [runtime state](../src-tauri/src/chat_commands/state.rs).

## R02: Thread and scheduler progress

**Failure pattern:** an `async` function performs blocking disk, crypto, OS-provider
or process work; a blocking worker retains a globally needed guard; or the parent
scheduler awaits optional work before polling unrelated ready operations.

Identify the executing thread and scheduling owner, not just the function's
signature. Move blocking work through the existing admitted worker boundary.
Inspect React render/effect/subscriber feedback loops and large synchronous data
transforms too; moving I/O off the UI thread does not bound those CPU costs.
Prepare bulk data outside shared commit guards, then reacquire the required
guards and revalidate before atomic publication. Keep critical sections small
without splitting a transaction or permitting stale side effects. Replacing a
mutex with an async mutex alone does not repair ownership or lock order.

Check that the UI event loop, accept loop and control scheduler can still make
progress while the worker is paused. Offloading a scan does not help other
branches of a scheduler if that scheduler still waits for the entire scan inside
one completion branch. Synchronous final persistence can still block; document
that boundary instead of claiming a timer can preempt it.

**Evidence:** pause actual expensive work after admission and assert availability
of the unrelated guard or operation. Release the pause and verify the final data.
See the attachment preparation and document-scheduler regressions in
[account sync tests](../crates/mesh-talk-core/src/node/account_sync/selection_tests.rs).

## R03: Actual work owns its lifetime

**Failure pattern:** cancelling an awaiter releases serialization or capacity while
its blocking worker, process or native provider continues; retries overlap, old
work writes into a new session, or shutdown waits for a worker while holding a
guard that worker needs to finish.

Name the owner of every task, timer, listener, subprocess, socket and permit.
Transfer required ownership into the actual work. Dropping an observer or timing
out a future is not evidence that external side effects stopped. Retain mutation
serialization and cleanup responsibility until real completion; bound admission
so repeated user retries cannot accumulate detached work.

Follow the existing retirement protocol: close admission, signal cancellation,
release guards needed for completion, join owned tasks and drain admitted work.
Uncancellable native work needs an explicit ownership boundary: retain the
operation's serialization ownership, but not unrelated runtime/session guards across
the provider call. Revalidate publication and compensate stale effects through
the owner. A timeout is not permission to abandon durable work or reuse its slot.

**Evidence:** cancel the caller while work is paused; confirm retirement progress
where promised, retained serialization, eventual cleanup and absence of stale
publication. Include repeated cancellation and a successful noncancelled case.
See [shared login tests](../src-tauri/src/shared_account_login/tests.rs) and the
runtime lifecycle boundaries in [module ownership](module-boundaries.md).

## R04: Deadlines cover the whole operation

**Failure pattern:** each route or phase has a timeout, but queueing, admission,
serial alternatives or retries make the complete user operation effectively
unbounded. A generic timeout error also triggers the wrong retry/recovery path.

Use the existing owning budget for the whole logical operation, including permit
waits and all alternatives; carry its remaining time into subordinate work where
needed. Distinguish observation deadlines, admission refusal and transport failure.
Preserve the owner-defined retry policy and original operation identity. Do not
reset the overall budget on every attempt or increase it just to hide a hang.

Check cancellation cleanup and late completion as well as elapsed time. Async
timers cannot interrupt a thread blocked in a synchronous mutex, native call or
filesystem syscall. Such a path also needs R01–R03. Budget values remain in the
[network contracts](network-design-reference.md#8-resource-budgets-compatibility-and-performance)
and implementation, rather than a second set of constants here.

**Evidence:** stall admission and a later network phase separately, use multiple
candidate routes, and verify permits/socket owners are released. Check that an
expired preflight has no partial durable mutation. Examples live in
[privacy runtime tests](../crates/mesh-talk-core/src/node/privacy_runtime_tests.rs)
and [group invitation tests](../crates/mesh-talk-core/src/node/account_group/tests.rs).

## R05: Bounded work must also make progress

**Failure pattern:** taking the first N entries every turn permanently excludes
older pending work; failed items pin a cursor; unbounded retries flood a queue;
or a completion signal arrives between checking a condition and registering a
waiter, leaving work asleep indefinitely.

Review fairness across successive turns, not only the batch size. Use the existing
cursor, coalescing and admission owners; distinguish independent lanes where
needed. Failed publication must not indefinitely prevent later eligible work.
Recheck conditions with the synchronization primitive's correct waiter protocol;
a notification alone is not the durable condition. Preserve startup, wake,
shutdown and missed-notification recovery behavior.

Keep independent reasons for continuing work separate. A worker can finish a
history page and then fail an optional favorites exchange; reporting that error
must not discard unfinished history or a refresh received during the exchange.
Consume a coalesced wake only when its follow-up remains scheduled after every
error branch. A failure without pending work must still avoid a busy retry loop.

Keep count, byte, task and concurrency budgets intact. Fair scheduling does not
guarantee a successful write into a full store, justify eviction of durable intent
or permit increasing capacity without reviewing consumers.

**Evidence:** create more eligible items than one batch, put pending work behind
terminal history, and include a failed item, cancellation and newly arriving work.
Assert progress and budget preservation across turns. See publication rotation
tests in the [group journal](../crates/mesh-talk-core/src/node/account_group/operations/journal.rs).
The `account_history_after_favorites_*` regressions in
[account sync tests](../crates/mesh-talk-core/src/node/account_sync/selection_tests.rs)
cover successful and failed optional exchanges, retained refreshes, unfinished
history pages and failure without pending work.

## R06: Exact authority and stale completions

**Failure pattern:** work authorized before an await or queue delay commits after
logout, same-user relogin, account replacement or a policy off/on transition.
Comparing only the username or current Boolean value misses changed authority.

Capture the owner's exact session/runtime installation and relevant revision at
entry. Revalidate through the existing atomic commit seam before side effects
and before publishing private results. Treat frontend generation guards as UI
ownership, not host authorization. Read commands can expose stale private data too.

Account for reentrant store subscribers and callbacks that can retire an owner
synchronously during publication. Lock once when combining sensitive proof and
revision checks. If a guard must be released around preparation, reacquire it and
check all invalidating conditions; an earlier successful check is insufficient.
Compensation must identify the stale operation's own effects and must not erase
newer valid state.

Scope presentation readiness and failure flags to the resource that produced
them, too. A component-wide decode failure can suppress a replacement image,
and a retired load callback can publish stale geometry. The
[inline media owner](module-boundaries.md#inline-media-presentation) binds both
outcomes to the current URL and session; the replacement-source and retired-load
regressions in [MediaPreview tests](../frontend/src/features/chat/MediaPreview.test.ts)
exercise recovery and rejection of stale callbacks.

**Evidence:** pause at the real commit seam; replace the session with the same
owner, change the relevant policy revision, or remove the source; then resume.
Assert no stale mutation or plaintext publication, plus a valid positive path.
The [desktop session owner](module-boundaries.md#2-feature-owners-and-shared-decisions)
and [runtime admission](../src-tauri/src/runtime_admission.rs) define the shared seam.

## R07: Observation is not mutation ownership

**Failure pattern:** a never-settling read leaves the UI busy forever; a timeout
is treated as failed durable mutation; clearing a busy flag allows duplicate
submission while the original native write continues; optional hydration hides
a known successful commit.

Use existing bounded read/observation helpers, retire stale listeners/timers and
offer a recoverable UI state. Preserve the actual mutation's ownership, ordering
and exact operation ID until its real outcome is known. Unknown admission must
remain unknown; reconcile or retry according to the owning idempotency contract.
Process a late definite refusal without fabricating a protocol terminal state.
Do not turn failed readback into an empty successful save or resend an accepted
message because optional roster, preview or notification work failed.

**Evidence:** hold IPC unresolved beyond the observation deadline, attempt a
duplicate action, then settle the original call with success and with refusal in
separate cases. Switch sessions during the wait. Assert bounded UI observation,
no overlapping mutation and correct late settlement. See
[group operation tests](../frontend/src/store/accountGroupOperations.test.ts),
[outgoing intent tests](../frontend/src/store/outgoingIntent.test.ts) and
[coordinated state owners](module-boundaries.md#coordinated-state-owners).

## R08: Durable state and failure semantics

**Failure pattern:** a read-modify-write race loses another update; optimistic
success precedes durable acceptance; retries duplicate writes; a failed/corrupt
read is silently treated as empty state; cleanup after a partial failure destroys
recoverable user data.

Identify the commit point, serialization owner and restart/replay behavior.
Prepare expensive work separately, then compare expected revision and lifetime
under the owning commit guard. Preserve operation IDs, ordering, atomic replacement
and existing compatibility rules. Multi-store changes are not automatically one
transaction; define recovery or compensation for every partial commit boundary.
Use established error types and distinguish absent, unavailable and corrupt data.

Preserve accepted intent when presentation fails. Report what actually committed;
do not relabel a committed change as rejected because later best-effort work
failed. Avoid empty catches, blanket defaults or unconditional retries that hide
the distinction. Follow the owning feature guide for rollback and durability.

**Evidence:** fail before and after the commit point, interleave a second writer,
retry with the same identity, and restart using retained data. Verify acknowledged
state survives, stale cleanup cannot delete a newer write, and partial outcomes
are reported honestly. See [development conventions](../specifications/development_conventions.md)
and the feature-specific owners in [module ownership](module-boundaries.md).

## R09: Bound input and resource use

**Failure pattern:** peer/UI-controlled data expands into unbounded allocation,
recursive traversal, decode work, fanout or task creation; a file path crosses its
allowed namespace; a convenient new IPC endpoint bypasses the existing admission
or authorization owner.

Validate at the owning trust boundary before allocation and side effects, using
existing frame, file, count, depth and byte limits. Preserve exact account/device
bindings and path/consent checks. Display metadata and frontend validation are
not authorization. Bound queues and retained retry state as well as active work;
do not remove backpressure to make a benchmark look faster.

**Evidence:** test the limit and just beyond it, malformed or missing data,
wrong-owner requests, saturation and recovery after capacity returns. Include
real loopback assertions for changed networking/crypto boundaries as required by
the [network review matrix](network-design-reference.md#minimum-feature-review-matrix).
Keep secrets and private payloads out of diagnostic logs and fixtures.

## R10: Evidence that exercises the failure

Use barriers, deferred promises, injected provider/storage stalls and controlled
clocks at the actual boundary. Prefer an observable progress assertion to an
arbitrary sleep or a test that checks only that a timeout helper was called.
Verify successful behavior, failure, cleanup and replacement through the same
production owner; a mock that bypasses the problematic lock proves little.

Give connection setup and capability negotiation their own bounded watchdog.
Start a progress deadline only after the required stalled operation is admitted,
and prove the competing work is still incomplete at that point. Release only
that work's barrier while retaining the stall. Measure cancellation separately
from setup and history progress; preserve each behavior's required bound.

A deadlock regression must not hang its own runner: bound coordination, arrange
fixture release even when assertions fail, and use an isolated process with an
outer deadline when a synchronous lock cycle cannot be safely unwound. An async
timeout inside the blocked thread is not sufficient. Avoid port-selection races
and process-global fixtures that assume serial execution; respect the repository's
test environment and resource controls.

Treat a lock-order suspicion, a reproduced bug and a passing regression as
different evidence. A stack sample or total suite duration alone does not prove
one test is deadlocked. Inspect the active operation, its duration and the actual
wait dependencies. Preserve required assertions when fixing flaky setup; do not
hide a failure with retries, larger sleeps, skipped tests or weaker selectors.

Record the tested source, command, environment, count and outcome. Reassess
consumers after merging new callers or changing a shared helper. Static scans,
passing unit tests and browser IPC mocks do not establish native OS behavior or
the absence of every deadlock. Cancelled, interrupted, empty and unavailable runs
remain distinct from passes. Completed individual commands do not turn a cancelled
parent run into a passing full gate. Follow the
[validation protocol](testing-feedback-loop.md#validation-stages-and-agent-responsibilities)
and keep unresolved native, cross-platform and stress obligations explicit.

## Review record

For applicable risks, make these answers reviewable in the PR or task result:

- **Owner and invariant:** which implementation and authoritative rule apply?
- **Risks:** which rule IDs apply, and what reachable failure path was checked?
- **Progress and lifetime:** what can wait, what stays available, and who owns work
  after cancellation, retirement or retry?
- **Commit and authority:** what becomes durable, what invalidates permission,
  and how are stale or partially completed effects handled?
- **Evidence:** which regression/scenario exercised the boundary on which source,
  and what actually passed, failed or remained unverified?

Use concrete paths, operations and assertions. “Thread safe,” “handles errors,”
“tests pass” and checked boxes without evidence are not sufficient explanations.
For a nonapplicable risk, a short reason is enough; do not manufacture tests or
ceremonial reports for unrelated concerns.

## Keep the guide useful as defects are found

The author of a confirmed defect fix owns the corresponding guidance update;
the reviewer checks it alongside the regression and owning contract. Follow the
[requirement maintenance loop](testing-feedback-loop.md#maintaining-standards-across-requirements).

1. Identify the missing general decision, not just the incident's file or symptom.
   Strengthen an existing rule when it already covers the mechanism.
2. Add a new stable rule ID only for a distinct, reusable failure pattern. Include
   **trigger, failure mechanism, required approach, authoritative owner and
   observable regression evidence**. Add its route to the selection table.
3. Update the owning implementation/guide and affected `validation.json` selectors,
   consumers or scenarios when needed. A prose reminder cannot replace the test
   or make an unselected regression part of future checks.
4. Maintain example links when code moves; update rule references when rules merge
   or retire. Do not silently reuse an ID for a different meaning. Remove obsolete
   advice rather than appending conflicting exceptions or dated status updates.

Keep this a compact set of reusable decisions. Do not add test-count histories,
commit diaries, machine-specific commands or a parallel copy of feature contracts.
Unconfirmed suspicions belong in an issue or ignored investigation notes. This
guide supports engineering judgment and executable validation; it cannot make
unreviewed generated code reliable by itself.
