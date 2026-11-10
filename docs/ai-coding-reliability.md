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

Branches of one `select!` share a task; adding runtime workers does not let a
blocked branch poll another branch of that same future. Independently scheduled
work must keep an explicit parent cancellation owner and retain the actual
child's retirement permit. The [inbound serving owner](../crates/mesh-talk-core/src/node/serving.rs)
is exercised by the real maintenance-hold Noise regression in
[account action tests](../crates/mesh-talk-core/src/node/account_group/tests/message_action_reference.rs),
alongside stalled-handshake retirement and the single-worker negative control.
Custom protocol and failed-initial-custody recovery fixtures must retain that
same independent host and retirement owner, including separate reactors for both
full-host participants and the synchronous history observer. The single-participant
connection-limit/retirement fixture likewise uses the production two-worker host;
retain its seven-slot saturation, original admission/retirement bounds and actual
socket closure checks, alongside the held-worker positive and single-thread
negative controls. This qualifies fixture capacity without attributing every
historical late admission to scheduler starvation. A source's synchronous durable append must not run on the receiver's
reactor. The growing-history regression in
[account sync tests](../crates/mesh-talk-core/src/node/account_sync/selection_tests.rs)
holds the actual source writer until a receiver page imports, then measures
continuation progress separately from setup and checks advancing cursors.
Full-host file fixtures retain a separate two-worker reactor for each simulated
application. The [linked reconnect fixture](../crates/mesh-talk-core/src/node/node_tests.rs)
also uses that owner at its original offline/online cuts and joins admitted work
before reopening a profile, on failure as well as success. This separates device
accept and maintenance loops; direct observer queries and public sends, including
caller-owned idle-expiry tasks, can still run on the observer reactor. A timer on
that reactor cannot bound a synchronous stall. Preserve the observation budget
and do not infer a particular deadlock or complete watchdog independence from an
unpolled timer alone. This includes the explicit account File result/retry rigs: use the
existing independent peer owner for source, requester, recipient and the launched
fresh sibling, keeping inactive retry phases and every byte/manifest assertion.
Release fault helpers before joining hosts, and preserve body failure after
actual retirement. Each online sender needs its normal delivery recovery owner
and an advertised address bound to its real listener. Local acceptance followed
by one best-effort flush cannot prove recovery after a transient exchange failure.
The [fresh-handshake and own-copy controls](../crates/mesh-talk-core/src/node/delivery_runtime_tests.rs)
exercise recovery of the same accepted message without resending; the latter
holds both sibling workers after its durable copy completes and requires target
confirmation while they remain held; unrelated devices cannot share that stall.

**Evidence:** pause actual expensive work after admission and assert availability
of the unrelated guard or operation. Release the pause and verify the final data.
See the attachment preparation and document-scheduler regressions in
[account sync tests](../crates/mesh-talk-core/src/node/account_sync/selection_tests.rs).

Async discovery observers must use the existing owned asynchronous peer-cache
snapshot and preserve persistence errors. The synchronous desktop convenience
snapshot writes storage and converts errors to an empty roster; polling it can
block the observer's reactor or misreport failed persistence as discovery latency.
The [account-login fixture](../src-tauri/src/shared_account_login/tests.rs) retains
its single signed loopback probe and original deadline, and a real cache-path
failure must preserve the cache API's error chain rather than time out. This
control proves error visibility, not the cause of an earlier hosted timeout.

## R03: Actual work owns its lifetime

Consume the canonical process owner's result contract rather than deriving a new
verdict from a diagnostic flag. Windows atomically terminates its owned job even
when empty, so verified `forcedRetirement` alone does not mean work leaked or a
successful test failed. Preserve the actual exit status and error, cancellation,
timeout and cleanup-verification results. The empty-job owner control and UI
runner outcome controls exercise that distinction.

Keep cancellation attached through owned log finalization. After command
closure, cancel unfinished log publication through the stream owner; partial
logs fail independently of the original exit status. Still join process-group
cleanup or verify the helper receipt: log closure cannot certify retirement.

POSIX process-group retirement proves only that group. A browser framework may
launch its web server in another group; use the actual tool's teardown protocol
and retain independent server-retirement controls in the UI command owner. Missing
cooperative teardown evidence cannot become verified cleanup merely because the
CLI exited or its group disappeared.
Tools such as Nextest and cargo-mutants create independently grouped children.
Killing their supervisor can close captured output while a real test remains
alive. Trace the tool's actual child ownership and shutdown protocol; neither
pipe closure nor the original group's disappearance proves retirement. The
[Linux mutation owner](../scripts/release/mutation-process-owner.py) uses a kernel
subreaper, pidfd-verified direct-child authority and one reaping loop across
normal exit and cancellation. If descriptor allocation fails, only a sole reaper
may signal a kernel-verified direct child whose status remains unreaped; consuming
that status before signalling would lose the identity guarantee. Child enumeration supplies candidates, not absence
proof; only kernel `ECHILD` after reaping establishes no owned descendants.
Missing, invalid or incomplete retirement acknowledgment keeps cleanup unverified.
Reserve the owner's bounded teardown window without changing ordinary callers'
defaults or using process names, stale PIDs or broad group sweeps as authority.
Actual direct-tool, nested-tool and active-build cancellation controls retain an
unrelated sibling and require acknowledged test retirement before returning.

Leader exit and output-pipe closure are separate lifecycle events. Bound draining
after owned-group retirement: an escaped descendant can retain inherited pipes
forever. Close only the owner's read ends, retain unverified cleanup and quarantine
its lease; the real detached-pipe natural-exit and cancellation controls exercise
this failure in the validation process owner.
Cleanup uncertainty is monotonic across cancellation and draining: successfully
retiring one group cannot clear an earlier unverified escaped-process boundary.
Retain the first failure while still awaiting every known retirement owner.

**Failure pattern:** cancelling an awaiter releases serialization or capacity while
its blocking worker, process or native provider continues; retries overlap, old
work writes into a new session, or shutdown waits for a worker while holding a
guard that worker needs to finish.

Name the owner of every task, timer, listener, subprocess, socket and permit.
Transfer required ownership into the actual work. Dropping an observer or timing
out a future is not evidence that external side effects stopped. Retain mutation
serialization and cleanup responsibility until real completion; bound admission
so repeated user retries cannot accumulate detached work.

A permit declared inside a blocking closure drops before the closure's remaining
captures. When those captures retain Node or storage handles, waking retirement
at that point can make close-before-reopen fail. Use the existing blocking work
owner in [runtime work](../crates/mesh-talk-core/src/node/runtime_work.rs) for
[file/media maintenance](../crates/mesh-talk-core/src/node/participant_runtime.rs),
so captures and undelivered results are destroyed before admission is released.
Keep the direct capture-drop ordering regression, queued cancellation and
undelivered-output controls, and the real native observer Node-release assertion;
do not narrow that assertion to conceal another producer retaining the profile.

A same-profile restart fixture must consume the existing retired storage close
before constructing its replacement. Joining producers through `stop()` alone
is not the fixture's proof that all Node and storage handles have closed. Keep
`stop_for_storage().await.close()` failure visible, including retained-handle
rejection, and preserve the subsequent durable reopen assertions. The shared
settings projection/restart scenario exercises this boundary with its old weak
application token still present; do not replace closure with a sleep or remove
the restart tail.

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
a notification alone is not the durable condition. Admission must observe both
permit release and transitions that make retained resources reclaimable. A one-time
pressure check can miss a busy connection becoming idle after the waiter starts.
The [session pool](../crates/mesh-talk-core/src/node/session_pool.rs) owns this
handoff: idle eligibility requests retirement, while the actual connection keeps
its permits until destruction. Preserve startup, wake,
shutdown and missed-notification recovery behavior.
Delivery fixtures reuse the register-before-check durable-status observer in
[delivery runtime tests](../crates/mesh-talk-core/src/node/delivery_runtime_tests.rs),
not periodic polling that competes with protocol and persistence workers. Keep
the exact message predicate and original deadline; the late exact-receipt
regression rejects unrelated wakeups, and the oversized-message regression also
retains pre-acceptance rejection and unchanged ratchet state.

Keep independent reasons for continuing work separate. A worker can finish a
history page and then fail an optional favorites exchange; reporting that error
must not discard unfinished history or a refresh received during the exchange.
Consume a coalesced wake only when its follow-up remains scheduled after every
error branch. A failure without pending work must still avoid a busy retry loop.

Retain demand throughout backoff. A future retry deadline makes queued work
temporarily ineligible; it does not consume that work. Wake the existing scheduler
at the earliest retained deadline when a worker is available, without relying on
periodic repair. A queued deadline must not spin while its worker is occupied.
The `account_history_capacity_backoff_retains_demand_without_periodic_repair`
regression in [account sync tests](../crates/mesh-talk-core/src/node/account_sync/selection_tests.rs)
checks real admission contention, retained work and the unchanged backoff.

Keep count, byte, task and concurrency budgets intact. Fair scheduling does not
guarantee a successful write into a full store, justify eviction of durable intent
or permit increasing capacity without reviewing consumers.

Finite preparation must survive unrelated ordinary appends without relaxing its
authority fences. Compare the owner's complete control and destructive revisions;
an unfinished derived index cannot certify a concurrent append. Capture completed
index readiness so later turns can progress. The ordinary-append and pending
observation regressions in [participant network selection](../crates/mesh-talk-core/src/node/privacy_sync/network.rs)
exercise progress alongside capability, proof, roster and file-policy invalidation.

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

A routing or capability preview must not become a second admission owner. When
the canonical reader reports a typed stale projection, preserve its existing
bounded preparation path before deciding authorization. Any cached local candidate
still needs full identity and current authority checks at the real commit seam.
The [forwarding owner](message-forwarding.md#source-admission-and-lifecycle) tests a
signed rename that needs preparation, plus dissolution and same-user/different-key
controls that must never produce a message or send receipt.

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

Keep inspection affordances separate from mutation permissions, and propagate
the containing view's restrictions into nested inspectors. Opening a reader must
not restore mutation authority. A bounded projection may omit existing data;
follow its owner's pending/remainder state rather than treating an empty visible
slice as an empty collection. The [reaction-summary contract](ARCHITECTURE.md),
[MessageBubble regressions](../frontend/src/features/chat/MessageBubble.test.ts)
and [inspector flows](../frontend/e2e/reaction-summaries.spec.ts) exercise these
boundaries with incomplete inline rows and read-only contexts.

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
do not remove backpressure to make a benchmark look faster. Callback-to-stream
adapters must reserve queue capacity before copying input. Closing admission must
also revoke buffered input and pending output; closing only the producer still
allows stale queued work to escape. The [BLE stream owner](../crates/mesh-talk-core/src/transport/ble.rs)
covers saturation/recovery and shutdown revocation in `transport::ble::tests`.

**Evidence:** test the limit and just beyond it, malformed or missing data,
wrong-owner requests, saturation and recovery after capacity returns. Include
real loopback assertions for changed networking/crypto boundaries as required by
the [network review matrix](network-design-reference.md#minimum-feature-review-matrix).
Physical execution resources need the same ownership discipline. A hosted runner
can exhaust disk while expanding a cache or compiling a second profile, before
it can publish failure logs. The [runner storage owner](../scripts/ci/runner-storage.mjs)
measures real capacity and reclaims only fixed disposable image SDK payloads;
[its filesystem controls](../scripts/diagnostics/runner-storage.test.mjs) protect
workspace/toolchain data, redirected paths and non-hosted environments.
Keep secrets and private payloads out of diagnostic logs and fixtures.

## R10: Evidence that exercises the failure

Scope fixture transformations and assertions to the owning configuration table,
not globally unique text. Test the same valid value in an unrelated profile and
verify that it stays unchanged. The [Nextest fixture owner](../scripts/diagnostics/test-fixtures/nextest.mjs)
and [cheap CI policy regressions](../scripts/diagnostics/ci-latency.test.mjs)
protect this boundary; actual compiler fixtures still exercise native behavior.
For scanner false positives, validate the exact path/value combination with the
real pinned scanner and retain detecting controls for another path, expression
and credential-shaped value. Keep exceptions in the [scanner owner](../.gitleaks.toml).

When a CLI wrapper exposes an existing policy selector, forward its literal
arguments to the authoritative checker and keep unrelated consumer arguments
separate. Exercise the actual entry point in every supported preparation mode,
including repeated selections and failed checks; do not replace a blocked
selection with a silent default. The [PR creation regression](../scripts/diagnostics/create-pr.test.mjs)
protects explicit ownership forwarding without changing acceptance authority.

Bind executable probes to the owning package, target kind and complete test
identity, and require exactly one runnable match. A matching binary basename is
not authority: Cargo can emit library and binary targets with the same name,
and metadata order can change. Assert the real probe executed its intended case;
a zero-test success cannot establish a negative control. The reordered ambiguous
metadata and archive-only missing-symbol controls in
[macOS archive qualification](../scripts/ci/macos-remaining-cli.test.mjs) protect
this boundary. Portable filesystem fixtures must construct native paths with the
platform path owner while retaining exact identity and escape checks; the
[archive diagnostics](../scripts/diagnostics/windows-coverage.test.mjs) own those
controls. Local execution does not replace actual Windows qualification.

Read transient browser state with one nonwaiting DOM snapshot when an outer
assertion already owns readiness. Attachment waits nested inside that assertion
can exhaust its budget before any decode or geometry observation. The
[media-scroll fixture](../frontend/e2e/media-scroll.spec.ts) establishes visible
preparation before releasing media and checks an absent row through a snapshot-only
observer; retain its exact geometry and cache assertions. Keep independent
negative controls outside the journey's active retention window.
The [group reading-anchor fixture](../frontend/e2e/group-reception.spec.ts) also
samples the log and row through one nonwaiting page evaluation, returning pending
when either is absent. Keep its existing assertion budget and exact ID/offset,
focus and unread checks; a slow locator operation is not proof of absent rows.

For repeated lifecycle or memory churn, distinguish cumulative input-driver waits
from the invariant being measured. The [memory guardrail](../frontend/e2e/memory.spec.ts)
keeps a trusted pointer round and final pointer navigation, then activates the
same DOM buttons through one browser-side journey for repeated replacement.
The [journey observer](../frontend/e2e/helpers/conversation-churn.ts) checks
committed DOM directly on mutations; animation frames are a fallback for layout-only
changes, not an extra prerequisite for already-visible state. Hold frames in a
browser control and verify success, the unchanged readiness deadline, observer/frame
cleanup and recovery. Keep per-activation timing diagnostics bounded and limited
to fixed fixture identities and elapsed times; aggregate timing does not identify
which rendering or scheduling dependency caused a hosted delay.
Require each exact visible active destination and loaded history, with bounded
readiness observers that retire their timers and callbacks. Retain the original
churn count, history size, watchdog and DOM bound. Do not replace UI activation
with direct store mutations; repeated DOM activation does not qualify trusted
input latency. An injected extra-bubble negative control must still fail the bound.
Successful progress before
a timeout does not establish why an individual driver operation was slow.

Keep extracted runtimes outside diagnostic artifact roots on failure as well as
success. Stage under an owned directory on the destination filesystem, retire it
in the extraction owner's cleanup path, and preserve unrelated build outputs. Diagnostic preparation has the same
ownership obligation: even empty directories can violate a later fresh-target
admission contract. Record directories actually created, remove only those that
remain empty, and preserve pre-existing files and trees on setup or execution
failure. The [Linux crash selftest lifecycle controls](../scripts/ci/linux-crash-diagnostics.test.mjs)
exercise this composition without weakening archive admission.
The [Linux archive fixture](../scripts/ci/linux-workspace-cli.test.mjs) rejects
extraction after real binary materialization and checks diagnostics stay free of
runtime binaries; successful-path cleanup alone does not prove this boundary. Bind archive-only application stack
file:line evidence separately from panic-header locations; Linux qualification
strips a copied ELF's debug sections as a negative control. Environment discovery
must return successfully before evaluating shell exports. The Linux wrapper
failure control rejects a real nonzero discovery status before admitting any
consumer, while strict aggregate collection retains every original default and
ignored receipt and genuine coverage cohort.

Source-integrity fixtures must not ignore tool output that production leaves
untracked. Keep generated evidence outside the source checkout and assert the
checkout stays clean after real tool execution; also prove unrelated untracked
source is still rejected. The [shared mutation controls](../scripts/release/run-mutants.test.mjs)
own this regression without weakening the production cleanliness gate.

Keep unrelated tool-version startup outside process-lifecycle assertions. The
[supervisor isolation regression](../scripts/diagnostics/windows-process-owner.test.mjs)
uses the existing version fixture while retaining real stage and output-limit
children, original watchdogs and verified completion receipts. Confirm the probes
were used; do not replace the process behavior under test. Check nested budgets
before treating an outer timeout as evidence that the inner operation hung.

Check name, App identity and source SHA alone do not establish execution scope.
A documentation eligibility decision is not product-test evidence, even when its
required job succeeds. Consumers must also validate the event, trusted repository,
branch and owning workflow/run. The release source owner binds checks to the
latest successful main-push suite; its PR-only, wrong-owner and stale-success
controls prevent a lightweight decision from authorizing publication.
Provenance is an additional constraint: filtering to a trusted suite must not
discard a newer failed or pending check that previously blocked acceptance.

Process-lifecycle fixtures must distinguish leader exit from group signal
permission. Observe the fixture-owned leader's process state when synchronizing
post-exit cancellation; a Darwin `EPERM` from a group signal probe does not prove
that leader is still running. The escaped inherited-output-pipe cases in
`scripts/diagnostics/validation-stage.test.mjs` own this regression and retain
separate assertions for cancellation, unverified pipe cleanup and escaped-child
retirement.

Source-order contracts must assert that each required command exists before
comparing its position. A missing substring returns a sentinel such as `-1` and
can otherwise make an ordering assertion pass vacuously. Match the canonical
command owner and keep actual execution controls for behavior beyond routing.
Negative controls must prove their intended input reached that boundary. For Git
scope tests, assert the real path and mode; a host that ignores executable bits
can otherwise produce an empty delta whose rejection passes the wrong assertion.

Browser fixtures must make hydrated backend settings agree with the intended
frontend state before navigation; a later settings response must not silently
remove the capability under test. The call-readiness fixture owns that setup.
For saved reading positions, exhausting bounded animation-frame polling does not
prove geometry is ready: retain the conversation-owned target and resume on
geometry changes, cancelling observers on cleanup and reader intent. Keep one
owner for automatic scroll correction, including library subscriptions that can
retain navigation across later measurements; a second writer can override valid
reader input even when the application owner rejects it. The viewport unit
regressions cover delayed geometry and insertion/resize pinning under reader intent;
real browser media scenarios qualify the virtualizer interaction.

Raw storage injection in concurrency fixtures must publish through the normal
owner notification before requiring a peer's unrelated scope to settle. Preserve
the protected in-flight page, exact inventory assertions and original latency
bounds; the same-source control fixture owns this preparation contract. Finish
unrelated readiness preparation before dispatching a timed network read. A
protected task can still reach its genuine idle deadline. Test barriers belong
before I/O, inside the owned child, with cancellation releasing their futures;
post-send hooks must not hide that deadline. Do not repeat long preparation waits
inside the measured held-read window.

Concurrent actual-tool fixtures must own output directories, not only source
and temporary data. A second Playwright invocation can otherwise clean a running
suite's traces. The browser command owner retains live logs and case progress,
joins descendants before reporting and fails on timeout or uncertain retirement.

Keep a test's observation scope equal to its named requirement. The
growing-history fixture measures exact durable originals during continuation,
then checks the full public
projection after retiring its continuously appending peers. Its projection read
must not compete with an endless writer inside the continuation latency bound.
Paused-clock maintenance tests wait for actual retry attempts and assert minimum
backoff spacing; real measured database duty time cannot imply a fixed number of
attempts inside an arbitrary virtual-time window. Preserve cancellation, other
maintenance-class progress and the no-early-retry assertions.

Prefer one qualified scenario for a requirement over repeated whole-host
variants with weaker preparation. The same-source control receipt owner retains
its five prepared partial-corpus Noise scenarios, including the exact live child
at admission, both preserved checkpoints, unchanged eight-second receipt,
cancellation and post-release recovery. Its former empty-corpus duplicate could
lose a natural read between setup notification and admission; removing that
variant does not qualify a separate fresh empty-corpus timing boundary. Keep the
retired-child observer and interrupted-preparation controls rather than adding
sleep, suppressing publication or accepting a stale child to rescue setup.
Its test-only dispatch gate must classify the actual owed parked pair credit,
not the shared resumed trace label. Capture that one-shot child before waiting
for its later projection; retain exact private-receipt evidence before release.
The paid-pair classifier regression rejects foreground resumes and unprotected
turns while the real Noise scenarios retain admission and recovery assertions.
When inventory ordering selects a different continuation path, construct canonical
signed fixture events with noncolliding deterministic identifiers and assert the
actual persisted key ordering. Distinguish durable commit, completed publication
and recipient installation in timing evidence; a publication-begin trace is not
proof of completion. Preserve bounded owner-phase diagnostics through recovery
without extending the measured deadline or warming away the required cold work.

A progressing serial test partition can exhaust its job watchdog without a hung
case. The Windows archive owner uses eight native serial execution partitions,
with direct required checks and the existing sealed once-only inventory contract.
Exercise actual pinned archive consumers and omission/duplication/failed-execution
controls. Preserve watchdogs, product assertions and full coverage. The existing
protection migration owner requires every successful current-head app-bound
replacement before retiring oversized partition names; skipped or partial runs
cannot qualify. Account for additional archive/setup cost and hosted variation.
The shared union verifier takes the caller-owned partition count; Windows capacity
changes must not alter the independent eight-part ASan contract. Run both real-tool
protocol fixtures and their missing/duplicate/failed-receipt controls. ASan
compilation reuse must bind the source, platform, compiler and archive/manifest
digests through trusted producer outputs. Reject changed bytes and inventories
before running archived tests; fresh-consumer controls must prove execution
without producer build outputs. Successful executable exit does not prove that
failure diagnostics survived relocation. Preserve the selected runtime and debug
artifacts, and require actual application stack frame file:line evidence after
removing producer outputs; a panic header alone is insufficient. Retain the
missing-symbol negative control in the existing
[archive fixture](../scripts/ci/macos-remaining-cli.test.mjs) and the
[CI archive contract](ci-maintenance.md). Retain real instrumentation checks on each runner.

A total functional browser-case watchdog is not a product performance SLA.
Trace the failed interval: when several individually successful cold-load,
rendering and interaction steps exhaust the total, use the existing generous
functional allowance consistently across platforms. Preserve individual
assertion bounds and explicit latency contracts. The real Playwright watchdog
control derives the actual source configuration, accelerates only its duration,
and requires timeout failure plus later-case execution with joined descendants;
never disable the watchdog or use retries to qualify a failure.

Independent message kinds should have independent browser scenarios. The
[automatic delivery fixture](../frontend/e2e/automatic-delivery.spec.ts) checks
text, sticker and file-card Awaiting/explicit projection separately; keep the
file disclosure and keyboard assertions in its file case. Three unrelated send
journeys can exhaust one functional watchdog before their final assertion.
Update the authoritative browser inventory alongside the split, retaining strict
trace/error evidence and every kind's confirmation predicate.

Functional browser scenarios should use the smallest data set that exercises
all named state transitions. Recovery-batch navigation needs distinct partial
and terminal deliveries plus concurrent live and duplicate signals; repeated
extra rows only multiply traced render work. Keep the separate large-history
window case and deterministic announcement-coalescing/deduplication tests in
[conversation reception](group-conversation-reception.md).

Use barriers, deferred promises, injected provider/storage stalls and controlled
clocks at the actual boundary. Prefer an observable progress assertion to an
arbitrary sleep or a test that checks only that a timeout helper was called.
Verify successful behavior, failure, cleanup and replacement through the same
production owner; a mock that bypasses the problematic lock proves little.

The group-inventory held-peer fixture rejects a real authenticated setup
exchange before admitting its independent publication measurement. Snapshot
exact-peer/group begin and end counters only after the actual Fingerprint hold
enters; require exactly one live round, unchanged counters and an open transport
through the original eight-second publication bound. A setup failure is not a
measured-window completion. A server task parked after client EOF is not a live
hold; observe transport retirement and join it on cancellation. The owner is
`group_inventory_publication_progresses_while_unrelated_peer_sync_is_held` in
`account_documents/tests.rs`, selected by core-account-group validation.

Give connection setup and capability negotiation their own bounded watchdog.
Start a progress deadline only after the required stalled operation is admitted,
and prove the competing work is still incomplete at that point. Release only
that work's barrier while retaining the stall. Measure cancellation separately
from setup and history progress; preserve each behavior's required bound.
An independent safety observer must also distinguish its pre-arm setup from
post-arm admission. Starting both deadlines when its thread is created lets slow
setup release the controlled gate before the asserted blockers enter. Keep each
phase bounded, begin its deadline at the corresponding handshake, and exercise
that transition with a controlled clock. The profile-rewrite retirement fixture
in `runtime_retirement_tests.rs` owns this observer regression.

A periodic probe can miss readiness between its last check and the deadline.
Use [R05](#r05-bounded-work-must-also-make-progress)'s waiter protocol to recheck
the actual condition on a transition, retaining the original deadline and bounded
fallback. A snapshot taken after timeout is diagnostic evidence, not success.

A bounded positive observer may keep waiting on the owning API's explicit pending
state. Match only that state; storage, authorization and unexpected failures stay
fatal. Preserve the original deadline and exact outcome assertions. Prior readiness
is not a lease: live sibling attachment recovery can invalidate a canonical history
projection between polls. The existing `ready_history` owner in
[`node_tests.rs`](../crates/mesh-talk-core/src/node/node_tests.rs) preserves
`StaleCursor` as pending and distinguishes a ready empty history; its storage and
authorization controls remain fatal. Keep the real checkpoint invalidation and
linked-device reconnect controls alongside those classification tests.
The source's positive message observation uses the same pending-state owner;
source readiness and sibling delivery share the original delivery deadline.
Timeout diagnostics retain pending/error results instead of unwrapping another
history query and masking the primary failure.

An offline fixture that needs prompt connection failure must own and observe
actual transport closure; reserving a TCP port does not guarantee refusal or
retire a pending dial. The [own-copy fixture](../crates/mesh-talk-core/src/node/delivery_runtime_tests.rs)
qualifies real accept-and-close behavior, joins its unavailable-endpoint owner,
then hands the same listener to the cold application without changing its
confirmation budget.

A direct protocol fixture must own its cold-start preconditions. Starting every
background repair loop can negotiate or import the claimed first request before
the explicit operation and consume its transport capacity. Reuse the bounded
[inbound connection owner](../crates/mesh-talk-core/src/node/serving.rs) when the
scenario drives requests directly; keep full-host recovery tests separate. The
[certificate refresh fixture](../crates/mesh-talk-core/src/node/account_documents/tests.rs)
requires no prior sibling negotiation or imported custody key, then positively
witnesses its real explicit negotiation and durable projection. It commits its
baseline without fanout and explicitly drives each real sync/Have exchange before
observing custody. A passive observer cannot advance pending work when the
fixture deliberately has no autonomous recovery owner.

Keep fixture validation at the narrow boundary it owns. A full-host scenario
whose only assertion concerns another test helper adds unrelated synchronization
and scheduling prerequisites without a product oracle. Reuse the real protocol
and business scenarios instead: [Wake authentication and recovery controls](../crates/mesh-talk-core/src/node/account_sync/selection_tests.rs)
own linked identity, revision deduplication and notification-driven convergence;
[custody scenarios](../crates/mesh-talk-core/src/node/account_documents/tests.rs)
own authenticated preparation, held-domain independence and durable custody.
Custody accounting preparation uses serial real channel-document requests on
inbound-only peers before autonomous workers start. A mutable whole-Optional or
Wake-settled snapshot is not a prerequisite for that accounting contract: it can
restart and create unrelated capacity contention while an observer waits. Remove
the global observer and its helper-only full-host tests; retain signed channel
floors, exact-peer capabilities and the real automatic custody/offline controls.
Do not add a clock switch solely to rescue a helper's self-test. Keep a loopback
business scenario focused on its named behavior. The member-invitation proof
ends after epoch delivery, pre-join isolation and role rejection. Existing
focused transfer/authority controls own ownership changes; the small two-node
departure proof owns actual offline pause/relaunch and retry of the original
signed event and parent cut. Do not chain unrelated controls and reconnection
requirements onto an already complete invitation scenario.

A fixture with a live authoritative identity observer must not simulate account
replacement by changing only a derived UI store. A later valid backend observation
can restore that store. Keep the [startup browser proof](../frontend/e2e/theme-picker.spec.ts)
focused on startup and real logout/relogin; the [scoped pack](../frontend/src/store/packs.test.ts)
and [linked appearance](../frontend/src/lib/linkedAppearance.test.ts) controls own
replacement and stale callbacks. A browser test of an actual account transition
must change its authoritative identity boundary consistently.

Keep a scheduling control's corpus at the protocol boundary it needs. The
[queued-source controls](../crates/mesh-talk-core/src/node/account_group/tests/selected_queued_source_control_latency.rs)
use a normal short partial page and retain strict result/checkpoint ordering;
separate real-protocol tests own full-page durability and replay. Qualify request
liveness before its cancellation trigger and the actual resumed owner before
admission. A cancellation-triggering publication need not leave the old request
alive after the publisher returns.

Browser device options express intent; they do not observe native input state.
Establish the required device mode through the browser's supported emulation
owner, then verify actual media queries and touch capabilities before measuring
responsive hit areas. Do not replace `matchMedia` with a JavaScript mock. The
[touch fixture](../frontend/e2e/touch-targets.spec.ts) exercises controlled native
state loss, actual CSS target sizes and a real tap through that same owner.
A negative control must observe the capability it removes; disabling touch does
not require every host to become hover-capable or shrink its CSS targets.

A deadlock regression must not hang its own runner: bound coordination, arrange
fixture release even when assertions fail, and use an isolated process with an
outer deadline when a synchronous lock cycle cannot be safely unwound. An async
timeout inside the blocked thread is not sufficient. Avoid port-selection races
and process-global fixtures that assume serial execution; respect the repository's
test environment and resource controls.

Intentional cancellation and output-limit controls need an independent process
owner. Inheriting the real runner's owner marker can terminate the enclosing
validation instead of only the synthetic command. The
[process-owner fixture](../scripts/diagnostics/windows-process-owner.test.mjs)
checks both the retained output-limit failure and successful enclosing supervisor
through an actual nested execution.

Executable process fixtures must copy their image and assert independent file
identity; a hard link aliases the live harness outside the child process owner.
The process-owner and release-reuse fixtures share bounded filesystem cleanup
after verified retirement, with a Windows held-image control that still rejects
a persistent lock and succeeds only after release.

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
parent run into a passing full gate. Verification collectors must release cancelled workflows while still rejecting
upstream failures. An unconditional collector can retain a cancelled candidate
and block its replacement in the concurrency group. The
[CI maintenance guide](ci-maintenance.md) owns this scheduling rule;
[merge-queue regressions](../scripts/diagnostics/merge-queue.test.mjs) exercise
eligibility after failure and ineligibility after cancellation. Follow the
[validation protocol](testing-feedback-loop.md#validation-stages-and-agent-responsibilities)
and keep unresolved native, cross-platform and stress obligations explicit.

Bounded subprocess output discovery must fail on truncation, not infer an empty
patch from a saturated buffer. Size Git diff transport for the supported source
boundary and exercise a real committed patch above the default buffer. Accepted
source promotion requires the existing exact main-push evidence owner plus live
source revalidation, not a local reference, PR-only green status or a coverage
threshold waiver. The [CI maintenance owner](ci-maintenance.md#synchronizing-an-accepted-main-source-into-dev),
`patch-coverage.test.mjs` and `pr-main-promotion.test.mjs` own these controls.

Dependency verification scope must bind identity to the actual tested merge and
consumer boundary, not only a bot author or version label. Independently revalidate
reduced profiles and require every selected producer's terminal success. External
coverage publication is a separate permission boundary from test execution; never
make missing bot credentials look like a product regression. The
[CI maintenance owner](ci-maintenance.md#dependabot-development-acceptance) and
`ci-docs-scope.test.mjs` / `ci-latency.test.mjs` own exact-source, mixed/native,
lockfile and failed/cancelled/missing-result controls.

Native failure evidence must include the actual release logger, not only captured
stdout. The [native capture owner](../scripts/diagnostics/native-capture.mjs) reads
bounded private file-log tails only after child retirement, preserves the original
failure when diagnostics are unavailable and rejects linked paths. Its filesystem
controls retain quotas, current-log priority and cutoff redaction. A responsive
failure screenshot or generic IPC timeout alone cannot identify a backend lock or
startup error; keep missing evidence distinct from a confirmed mechanism.

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
