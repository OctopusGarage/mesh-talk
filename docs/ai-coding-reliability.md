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

Cross-cutting idle budgets, demand-driven work and accumulation rules belong to
[performance requirements](performance-requirements.md). Apply them alongside the
correctness rules below; a smaller resource chart does not prove preserved recovery.

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
Account archive snapshots, history projections and delivery recovery share the
plaintext-sidecar order: sent, received, then received files. Recovery must not
hold received while waiting for sent: an archive has no delivery guard and can
otherwise hold sent while waiting for received. The real recovery regression
`recovery_does_not_hold_received_while_waiting_for_sent` in the
[recovery owner](../crates/mesh-talk-core/src/node/delivery_runtime.rs) holds sent,
probes received at the actual acquisition boundary, then releases contention and
joins the worker before asserting replay and progress.
Archive revision snapshots must read and release device-sync policy before
acquiring plaintext sidecars. Attachment commit holds delivery and policy while
checking the live received-file card; an archive holding received files while
waiting for policy creates a cycle and can also block erasure behind delivery.
Keep archive-key revalidation after indexing and page selection so a changed
policy still invalidates the export. The regression belongs to the
[account archive owner](../crates/mesh-talk-core/src/node/account_sync/archive.rs)
and must qualify progress while the competing policy/sidecar boundary is held,
then release and join both owners before asserting the outcome.
The same boundary applies to file-proof revisions: identity import holds the
proof store while validating received-file rows. Archive snapshots must read and
release proof metadata before taking sidecars, and retain proof revision in the
final archive-key validation. Its separate held-proof regression qualifies the
actual archive path; policy-only coverage does not establish this lock order.
Do not build a revision array from chained mutex temporaries: guards can survive
until the entire array statement ends. Snapshot each value in a completed
statement before acquiring the next metadata lock. The archive's held-identity
control must also prove suppression is free, preserving the identity-then-
suppression order used by file-row visibility checks.
Avoid invoking arbitrary callbacks while guarded. Where a commit callback must
run inside an authority guard, keep that contract narrow and inspect its complete
call chain. Queue presentation without retaining a guard needed by the UI;
validate captured authority when the queued callback executes.

Rust [temporary scopes](https://doc.rust-lang.org/reference/destructors.html#temporary-scopes)
can keep guards alive through the complete statement, including struct literals
and tuples. Read repeated owner scalars under one explicit short guard,
then release it before constructing a value that could acquire the owner again.
Check the actual temporary scope before declaring a cycle: each operand of a lazy
Boolean expression has its own temporary scope. An explicit short block can make
an observation boundary clearer without changing an already-released guard.

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

Evaluate authoritative cheap decisions before expensive fallback work. An exact
accepted identity binding or direct current policy grant must not first scan
unrelated history to establish an alternative grant. If policy can change,
recheck at the write owner; release its guard before obtaining missing fallback
evidence. The [privacy admission tests](../crates/mesh-talk-core/src/node/privacy_runtime_tests.rs)
require real certified-sibling SessionReady while unrelated histories are held,
and exercise policy tightening with and without genuine shared-group authority.

Check that the UI event loop, accept loop and control scheduler can still make
progress while the worker is paused. Offloading a scan does not help other
branches of a scheduler if that scheduler still waits for the entire scan inside
one completion branch. Synchronous final persistence can still block; document
that boundary instead of claiming a timer can preempt it.

Trace guards inside a helper named `blocking`, not only the caller. A read-only
image decode must not hold the global session/app-lock guard throughout its CPU
work. The [preview owner](../src-tauri/src/chat_commands/files.rs) retains the
runtime in a detached parent until its actual worker joins, checks one admission
deadline before queued work starts, and revalidates lock revision before
publication. Its held-worker regression checks unrelated session progress and
that observer timeout does not release worker capacity or runtime ownership.

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

Reuse an immutable certificate binding through the owner that validated it on
admission and reopen. A hot membership query must not clone/sort the full roster
and reverify signatures per row/page. Keep exact account and exchange-key checks,
and retain current policy/revocation fences. The
[roster regressions](../crates/mesh-talk-core/src/node/account_devices.rs) count
actual verifier calls through `Node::certified_sibling` while rejecting forged
admission/reopen records and substituted exchange keys.

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
Revoking authorization must not erase the active worker's ownership token before
its matching completion releases runtime bookkeeping. Discard inactive scheduling
hints separately from retiring actual work and its permits.

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
Browser observations carry the caller's remaining budget into the browser, then
use a monotonic deadline before and after DOM sampling. A delayed timer cannot
make a late visible sample timely. The [group preparation controls](../frontend/e2e/group-reception.spec.ts)
and [memory controls](../frontend/e2e/memory.spec.ts) reject expired callers and
samples that consume the remaining time; clock overrides stay in synthetic
protocol controls, separate from application journeys.

**Evidence:** stall admission and a later network phase separately, use multiple
candidate routes, and verify permits/socket owners are released. Check that an
expired preflight has no partial durable mutation. Examples live in
[privacy runtime tests](../crates/mesh-talk-core/src/node/privacy_runtime_tests.rs)
and [group invitation tests](../crates/mesh-talk-core/src/node/account_group/tests.rs).

## R05: Bounded work must also make progress

For change-driven receive, subscribe before observing the predicate and retain
that observation until all captured work is handled. A notification invalidates
quietness; it must not cancel valid unrelated work or acknowledge delivery. Keep
one coalesced follow-up for changes during a drain. Observation tokens may outlive
a connection: do not hold an idle socket across many individually bounded steps
whose aggregate exceeds its between-request deadline. The
[receive owner](../crates/mesh-talk-core/src/node/serving/mailbox_watch.rs) and
[loopback receive regressions](../crates/mesh-talk-core/src/node/delivery_runtime_tests.rs)
cover quiet preparation, prompt arrival and local policy invalidation; the
[watch protocol tests](../crates/mesh-talk-core/src/node/session/mailbox_watch.rs)
cover timeout, reconnection predicates, EOF and bounded capacity. Keep their
acceptance limits distinct from measured native CPU and power.

When a state publication reschedules an actual-deadline timer, already-due work
must not depend on another timer tick after the original callback is cancelled.
Dispatch the due work after the current state notification, coalesce repeated
publications, and recheck the owning lifetime before mutation. The
[chat expiry regressions](../frontend/src/store/chat.test.ts) cover a due deadline
interrupted by state publication and cancellation before its queued dispatch.

Distinguish a fully checked no-work outcome from missing evidence, deferred
preparation and failure before parking repeated work. Learn scheduling-only
absence after source guards release and exact before/after inputs match; keep
fresh direct reads and bounded repair. Negative hints must not displace useful
confirmed work. The [consent owner](../crates/mesh-talk-core/src/node/file_consent.rs)
returns only its explicit pending/no-published-control outcome; the
[mailbox regressions](../crates/mesh-talk-core/src/node/opaque_relay/incremental_tests.rs)
and [bounded hint pressure tests](../crates/mesh-talk-core/src/node/opaque_relay/incremental/controls.rs)
retain decision changes, direct Pending reads, failures, scope changes and the
positive cache's existing capacity.
A domain-wide paging upper is not a mailbox revision. Unrelated mailbox appends
must not invalidate settled work when its exact mailbox binding/revision and
local source remain unchanged; retain rollback rejection and the original
expiry deadline. The real send-only eight-offer regression in the same mailbox
owner advances another mailbox through a genuine encrypted relay append, while
same-mailbox growth restores preparation. Trace classification must use the
same predicate as ordinary scheduling because it can govern actual work.

Do not interpret an unavailable dependency or a generic stale-cursor error as
proof of finite preparation progress. Preserve the launch deadline when no rows,
edges or preparation phase advanced. Register dependency waiting before probing,
so a concurrent readiness notification cannot be overwritten by the attempt.
After consuming readiness, reserve the active network attempt's cancellation
deadline before awaiting; an earlier wake must not erase its backoff.
Publish scoped readiness hints after installing the state consumers observe and
releasing source and cache guards; the next attempt still validates authority.
An unchanged reception completion is not dependency publication and must not
clear a blocked deadline. Keep queue-continuation booleans distinct from state
change evidence: completing the last queued lane can still publish readiness.
The
[group-file authorization regression](../crates/mesh-talk-core/src/node/group_file_authorization_tests.rs)
holds a real stale checkpoint, checks quiet backoff, then drives the actual
checkpoint owner and verifies continuation before the deadline expires.

Bound work across owners as well as inside a page. One physical mailbox must not
be rescanned by every outgoing attachment merely because its export conversation
differs. The [publication regression](../crates/mesh-talk-core/src/node/group_file_authorization_tests.rs)
drives actual indexed file and immediate group-control custody and requires zero
unrelated receive steps, unchanged
old duplex receipt bytes, renewed custody after relay replacement, and refusal
processing through the independent drain. Its history-publication intent retains
identity supplements and fresh authority/remote checks under existing quotas;
legacy duplex behavior remains separate. Prefer a purpose-specific local hint
namespace over letting an outgoing worker rewrite incoming runtime or repair
bindings in a shared envelope.

**Failure pattern:** taking the first N entries every turn permanently excludes
older pending work; failed items pin a cursor; unbounded retries flood a queue;
or a completion signal arrives between checking a condition and registering a
waiter, leaving work asleep indefinitely.

A producer revision gate must precede expensive routing filters and projection
hydration. A completed last item cannot acknowledge earlier failed or deferred
items: require matching settled evidence for the whole selected cycle. The
[channel maintenance regressions](../crates/mesh-talk-core/src/node/channel_account_maintenance.rs)
exercise multi-channel deferral, failed capability cooldown and a new peer behind
a quiet cursor. Sparse integrity repair rotates one bounded page; only new demand
may drain the available backlog, as checked by
[local maintenance](../crates/mesh-talk-core/src/node/account_sync/local_maintenance.rs).

Keep retry scope equal to the observed success scope: one destination's custody
cannot quiet a peer's other queued destinations. The
[delivery runtime](../crates/mesh-talk-core/src/node/delivery_runtime.rs) owns
peer failure pacing separately from bounded exact-destination quiet hints;
its real relay-replacement regression keeps both prequeued events eligible.

Local admission pressure is a deferral, not remote failure or successful
progress. Preserve learned failures while releasing only the exact reservation;
only actual success or changed routes/authority may clear that evidence. The
[private probe owner](../crates/mesh-talk-core/src/node/privacy_routing.rs) and
[retry schedule tests](../crates/mesh-talk-core/src/node/delivery_retry.rs) cover
contention after repeated failures, bounded deadlines, immediate route recovery
and rejection of stale completion tokens.

Per-producer limits cannot protect a shared socket pool when several owners dial
the same historical inventory. Classify speculative work at shared admission,
retain exact identity/endpoint/generation and finite freshness for positive hints,
and leave capacity for current routes. A busy speculative lane must not hide later
fresh peers or permanently rewind the same failed prefix. The real TCP regression
in [privacy runtime tests](../crates/mesh-talk-core/src/node/privacy_runtime_tests.rs)
starts independent producers against stalled listeners and requires bounded
historical sockets plus a healthy Noise/sync round within its original deadline.
The [session pool tests](../crates/mesh-talk-core/src/node/session_pool.rs) cover
actual owner cancellation and bounded, expiring, scoped hints.

An unchanged remote delta does not by itself suppress repeated local control
construction. Qualify individual already-confirmed items before historical body
and file checks, using a conservative local mutation token plus exact fresh
remote evidence and a finite external-state fallback. Keep the rotating selector
running; a memo for one batch cannot certify its unexamined suffix. Such hints
are runtime work suppression, never reusable authorization or custody. The
[incremental control tests](../crates/mesh-talk-core/src/node/opaque_relay/incremental_tests.rs)
require zero repeated offer checks/HAVE requests under unchanged inputs, immediate
policy-change publication and reconstruction after replacement behind the same
relay identity; [hint tests](../crates/mesh-talk-core/src/node/opaque_relay/incremental/controls.rs)
cover expiry without renewal, exact scope and bounded eviction.

Match a mutation token to the data that actually owns the result. A connection's
total SQL change count includes in-memory TEMP work; a MAIN file version can
advance even for a zero-row UPDATE. Neither alone proves changed
producer data. Within one open, exclusively leased writer connection and a finite
hint lifetime, pair the MAIN version with the non-reset pager dirty-write count;
recompute when both change. Unsupported observations or a faulted owner cannot
supply unchanged evidence. This is not a multi-writer or durable revision scheme.
The [database observation test](../crates/mesh-talk-core/src/storage/database.rs)
requires unchanged evidence after reads, TEMP work, zero-row MAIN updates, invalidation after real dirty-page writes
(including indexed same-value updates), and rejection under the fault
fence. The real control exchange above injects TEMP work and a no-op MAIN update
between unchanged rounds.

Concurrency limits do not suppress duplicate retries across independent producers.
Keep actual failed-endpoint pacing at the shared authentication owner, scoped to
the full device binding and recovery generations; never cool untried routes or
interpret local capacity as remote failure. A caller's cancellation must release
its own reservation without modifying a replacement. The real TCP cross-caller
regression in [privacy runtime tests](../crates/mesh-talk-core/src/node/privacy_runtime_tests.rs)
requires one failed connection under repeated ordinary callers and immediate
recovery on network or fresh signed discovery changes. Session-pool tests cover
bounded hints, mapped endpoints, cancellation and late reservation retirement.
Repeated expected background failures should use bounded diagnostic aggregation;
reporting must neither change retry state nor require an idle reporting timer.

Idempotent retirement requires an unchanged-value predicate at the SQL owner.
Setting an indexed status to its current value can dirty pages, even if a later
cache compares page-write counts correctly. Restrict retirement to active rows;
reopening or releasing the last lease must not rewrite already-retired facts.
The [channel checkpoint tests](../crates/mesh-talk-core/src/node/channel/checkpoint/tests.rs)
require abandoned-query recovery, zero warm-open changes with 1,024 retired rows,
and no indexed page writes when a completed query's final lease is dropped.
Opt-in diagnostic emission must also work in production log configurations;
verify the target filter and real release output rather than relying on INFO
macros that can be compiled out.

A deferred cursor with no remote continuation still owns local work. Retain it
in the scheduler queue beneath its exact retry deadline; retaining a timestamp
in an unreachable checkpoint loses recovery. The account document contention
regression in [document tests](../crates/mesh-talk-core/src/node/account_documents/tests.rs)
requires one attempt under repeated blocked wakes and automatic recovery after
admission is released, with unchanged recovery deadlines and joined fixture work.

Bounded caches must preserve useful unfinished work between turns. Protecting
only actively borrowed entries lets rotating demand evict every partial owner,
retire its SQL selection and restart it on the next attempt. Defer new admission
while admitted owners make progress; reclaim completed, revoked or genuinely
stalled work through a finite progress deadline. Access, validation and repeated
unchanged responses do not renew that deadline. Drop evicted owners outside
cache guards when destruction can retire database work. Treat a contended entry
as unavailable during admission rather than waiting for its SQL worker while
holding the global cache guard. Classify lightweight receive/control demand
separately from heavy historical preparation so productive bulk work cannot
reserve every admission slot needed for incoming messages. The fifth-scope pressure
and stale-progress controls in the
[mailbox owner](../crates/mesh-talk-core/src/node/opaque_relay/incremental_receive_tests.rs)
qualify the actual selection identities, write commits and eventual completion.

Pending work in one lane is not successful progress in another. A failed exchange
must retain demand and a future retry deadline rather than inherit a continuation
from unrelated favorites or documents. Ordinary notifications cannot bypass that
deadline. The real, tick-disabled [account history failure regressions](../crates/mesh-talk-core/src/node/account_sync/selection_tests.rs)
combine favorites failure with authenticated EOF or negotiation timeout, then
assert quiet backoff and eventual retained-work recovery.

Retaining a failed completion does not justify notifying its own retry waiter.
The media completion regression in
[delivery runtime tests](../crates/mesh-talk-core/src/node/delivery_runtime_tests.rs)
fails the actual persistence owner, preserves the exact pending revision and
requires backoff until a real consent event wakes recovery. Keep received-byte
progress notifications separate from unsuccessful completion attempts.

Review fairness across successive turns, not only the batch size. Use the existing
cursor, coalescing and admission owners; distinguish independent lanes where
needed. Failed publication must not indefinitely prevent later eligible work.
Distinguish volatile discovery progress from durable acceptance. Rotating an
unchanged historical page must not require a durable write on every poll. Where
a cursor is only a replay hint, bound checkpoint frequency while preserving
fairness across restarts, exact owner fences and retry after checkpoint failure.
An in-memory cursor must not acknowledge a failed required checkpoint or override
a newer durable cursor written by another owner.
Observing a completed projection must not manufacture and immediately retire
durable work on every poll. Qualify quiet reads against the owning source and
generation, while preserving durable demand before any preparation can mutate
or yield. Cover source changes between preparation and installation, not only
an unchanged single-threaded read. Historical membership cuts must retain their
completed indexed fact lease rather than rebuilding per-event checkpoints for
unchanged roots. The [causal checkpoint owner](../crates/mesh-talk-core/src/node/channel/checkpoint/causal.rs)
qualifies bounded completed cuts by repository binding, exact canonical source,
root set and inclusion mode; every hit rechecks the account lifetime and source.
The `channel_completed_causal_cut_` regressions assert quiet SQL mutations,
source and account invalidation, failed-construction retry and borrowed eviction
leases. Current caller authorization and discovery proofs remain fresh.
Compaction and replication must not form a feedback loop: freeze the eligible
commit cut for each destructive epoch so later rehydration cannot repeatedly
reopen the same deletion work. Fresh semantic demand must advance and fence the
cut even when the request already exists. Keep parent-reference checks current;
processed presentation is not retained raw custody. The automatic-maintenance
round-trip regression in the [participant network owner](../crates/mesh-talk-core/src/node/privacy_sync/network/incremental_tests.rs)
qualifies legacy replay, eventual quiet and a new Profile epoch. Completed
[media GC](../crates/mesh-talk-core/src/node/media_store.rs) also remains quiet
until a new schedule, including after completion failure and retry.
A clean network exchange is not evidence of new data. The
[delivery runtime](../crates/mesh-talk-core/src/node/delivery_runtime.rs) keeps
advanced, quiet and deferred reconciliation distinct: empty success must not
reset a fast progress deadline, and a failed route still cooling must not become
quiet success. Retain successful framing/session reuse separately. Qualify
quiet deadlines with the admitted route and policy generation; refreshing an
unchanged subscription must not invalidate them. The real Noise
`clean_delivery_reconciliation_is_quiet_and_later_remote_data_advances` regression
covers empty success, later data, unchanged replay and deferred relay recovery;
`quiet_pull_deadline_and_changed_admission_do_not_requeue_as_progress` covers
actual scheduling policy. A repeated positive custody proof is also not fresh
progress while the durable delivery destination still awaits its receipt. The
`unchanged_direct_custody_does_not_keep_destination_retry_hot` regression drives
the real delivery loop, retains awaiting state, bounds repeated positive
confirmations and admits another new message before the first receipt arrives.
New retry demand belongs to durable intent acceptance, not successful polling
or idempotent reacceptance of an existing pending intent. Settled reception
retirement suppresses same-value
writes inside its existing transaction while preserving cleanup after a committed
ACK and failed retirement, as covered by the
[reception-effect tests](../crates/mesh-talk-core/src/node/reception_effects/tests.rs).
Separate due retries, cooled jobs and unfinished discovery or preparation.
Reserve a retry deadline before cancellable work and retain its wake through the
existing scheduler. Scope each reservation to the exact attempt and eligible route;
a late completion must not cool or release its replacement, including after a
route changes away and back. Bound retained reservations without evicting active
or unexpired entries to admit new work. Start failure backoff from completion,
and distinguish a successful clean protocol step from an unchanged phase name.
A future deadline must not force fast polling; expired
deadlines must not create zero-delay spins. Finite discovery still needs short
turns while admission capacity remains, even when every queued job is cooling.
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

Separate remembered routes, fresh availability and synchronization demand. A
certificate or cached endpoint permits neither an assumption of online presence
nor endless bulk retries. Scope private authenticated availability to the exact
identity, current authority and a finite deadline; public invisibility must not
disable authorized private recovery. A repeated live heartbeat is not new data,
while expired-to-fresh evidence at the same endpoint is a reconnect. Preserve
receiver-owned partial comparisons while offline, and renew productive work only
from admitted current-scope evidence. Local publication should hint receivers,
not restart pulls from every sibling. The availability and local-notification
regressions in [account sync tests](../crates/mesh-talk-core/src/node/account_sync/selection_tests.rs)
exercise this separation alongside the sparse-repair owner.

A nonempty pending queue does not certify that its dependency is available.
Keep local paginated preparation prompt, but let blocked file work back off;
meaningful route, availability, group metadata and consent changes wake the
existing producer. Subscribe before work so changes during an exchange survive
until its wait, and treat a lagged group subscription as missed invalidation.
The blocked-receive, group-wake and lag regressions in
[participant runtime tests](../crates/mesh-talk-core/src/node/participant_runtime_tests.rs)
cover these boundaries alongside heartbeat filtering and the original deadline.
Discovery cursors are not consent or delivery evidence: cyclic old-input rotation
must not checkpoint a lower watermark. The monotone rotation and failed-write
regressions in [file consent storage](../crates/mesh-talk-core/src/node/file_consent_store/indexed.rs)
preserve quiet rotation, restart progress and exact owner fencing.

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

A completed negative decision can be reusable work too. Prove that the mutation
preserves its inputs and can only move its threshold farther from eligibility;
rebind it atomically with that mutation. Include indirect dependencies: referencing
a protected leaf can change which older leaf is protected. Partial, stale or
positive decisions retain their original invalidation and publication rules.

Create durable demand from changes to its actual inputs, not from a callback that
can also report an unchanged round. Commit required demand before acknowledging
discovery, and preserve idempotent retry across either commit failing. When older
cursors did not certify the new obligation, supply a bounded, one-time backfill
without discarding completed facts. The unchanged-conversation callback and
request-before-cursor regressions in
[node tests](../crates/mesh-talk-core/src/node/node_tests.rs) and atomic bootstrap
regression in the [receive owner](../crates/mesh-talk-core/src/storage/profile/profile_receive.rs)
exercise this boundary.
An empty receive queue must not update an unchanged fair-turn ticket either.
When a bounded lookahead proves that the entire scoped queue was selected,
rotating all unfinished rows preserves the same order and needs no durable
mutation. Retain partial-page rotation, including oversized attempts, and
terminal deletion. The complete-page and partial-page regression in the
[receive repository tests](../crates/mesh-talk-core/src/eventlog/repository/tests.rs)
checks fairness and SQL/commit/body-read costs together. The settled ordinary and
control callback regression in [node tests](../crates/mesh-talk-core/src/node/node_tests.rs)
checks SQL mutations, committed writes and body reads together.
Within one operation, traverse a shared bounded source once rather than
reconstructing its SQL iterator for every candidate peer. Retain only the
already-bounded candidate metadata; do not collect historical scopes or turn
advisory selection into a grant. Keep fallible pages, current membership and
replica checks, and final exchange authority. The [file recovery regressions](../crates/mesh-talk-core/src/node/serving.rs)
count real offer/scope queries across multiple peers and retain direct fallback,
read failure recovery, replica opt-out, membership removal and source rotation.

Compare repeated exact queries with their first preparation too. A metadata-only
probe cache does not eliminate selection reconstruction in a different request
path. Reuse an exclusively owned, bounded prepared view only under its complete
current qualification; recompute mutable answers and preserve durability and
authority checks. The exact-custody regression in the
[participant network adapter](../crates/mesh-talk-core/src/node/privacy_sync/network.rs)
checks unchanged present/missing queries, unrelated and same-conversation appends,
authority revocation and selection release on disconnect. The ordinary Request
regressions in the [paged session tests](../crates/mesh-talk-core/src/node/session/paged_tests.rs)
require unchanged warm requests to perform no SQL mutations or commits, while
source appends, changed remote Have and revoked authority remain fresh. Model
production lifetimes: keeping a fixture's adapter or socket alive can hide
request-local or connection-local preparation churn. The requester regressions
create fresh adapters and fresh authenticated sockets while the source stays
unchanged. Node-owned indexes are checked out exclusively under complete version
and authority qualification; parked indexes have a separate bounded budget and
cannot consume or bypass active query admission. The selection-owner regression
checks quiet park/reactivation, preserved capacity and rejection by another
repository owner. Runtime retirement closes publication before extracting cached
plans and owns their real SQL cleanup through the final work-permit release. Protocol success also does not
imply received data. The production-serving metadata regression preserves a
pending, not-yet-decryptable file queue and verifies that a fingerprint query
cannot rotate its tickets; accepted/duplicate events publish a consumable reception
signal and invalid events do not. Consume the signal outside its store guard
before invoking message/file/profile owners. A failed batch can still contain an
accepted prefix: dispatch its actual conversation scopes on error as well as
success. Cancellation cleanup must retain both runtime retirement ownership and
the original connection capacity until reception finishes; a retirement counter
alone does not bound concurrent workers. The partial-Followup regression in the
[paged session tests](../crates/mesh-talk-core/src/node/session/paged_tests.rs)
injects a real second-record storage failure, while the
[serving controls](../crates/mesh-talk-core/src/node/serving.rs) hold reception work
through cancellation, timeout and dropping outside the runtime context. Apply this
to both transport roles: a file-manifest requester can durably import a chat reply
before checking download consent. The [delivery regression](../crates/mesh-talk-core/src/node/delivery_runtime_tests.rs)
requires automatic plaintext and the exact recipient receipt after that real
bidirectional exchange, without a history read or another payload healing it. Reuse
completed export preparation under its current source stamp. Require exact Have
when ancestor closure depends on it; an independent plan may instead rebind its
cursor digest and apply the fresh Have as a read filter, without persisting query
IDs or rebuilding flags. The real TCP regression alternates Have sets and requires
correct fresh responses with zero SQL mutations. Unprepared or closure-dependent
views retain their existing staged preparation. In every case,
authority-only view validation deliberately permits a finite old source cutoff.
The same selected page may be saved repeatedly without advancing its resume
context. Keep source and view checks inside the owning transaction, but update
the stored context only when its value changes. The selection preparation
regression checks zero additional mutations/commits and rejects a retired view
even for a matching context.
Do not leave a completed lookup reconstruction classified as protected unfinished
bulk work. Retire a selection created only for that finished lookup without
marking bulk coverage complete or discarding a preexisting partial upload. The
[six-scope mailbox regression](../crates/mesh-talk-core/src/node/opaque_relay/incremental_tests.rs)
resolves a target outside the bounded recent-target memo across more scopes than
heavy view slots; actual unfinished-upload pressure retains its separate oracle.
Retain an ordinary responder view only after all response frames succeed, and
test terminal-send failure and cancellation while its store remains alive.
A reconnect or lost runtime memo must not manufacture another immutable event
for unchanged content. Randomized sealed ciphertext cannot establish plaintext
equality, and the sender may no longer possess the ephemeral decryption secret.
Keep any bounded sender fingerprint and canonical event in the same transaction;
recheck the retained original, full identities and required dependency on reuse.
The [profile publication regression](../crates/mesh-talk-core/src/node/account_profile_io_tests.rs)
compares actual retained IDs and source revisions across retries and reopen.
Measure repeated cryptographic work separately from writes: stable event IDs and
zero SQL mutations do not prove quiet CPU. A positive intrinsic verification memo
must bind the complete serialized input, including its signature, and be bounded
independently of retained history. Read current ownership and selected data before
lookup; recheck requested identities and mutable authorization on every use.
The [indexed consent tests](../crates/mesh-talk-core/src/node/file_consent_store/indexed.rs)
count real signature invocations and cover same-ID tampering, requested identity
changes, fixed-capacity eviction and re-verification, alongside policy and owner
recovery tests.
Apply the same rule to empty selected federation slices: validate session,
completion and resource authority, but do not rewrite unchanged session/global
charges. The raw-selection regression in the
[federation staging owner](../crates/mesh-talk-core/src/postoffice/storage/federation/tests.rs)
checks mutation and commit counts while preserving framing and revoked-session
rejection.
A durable version must not be invalidated solely by a new process identifier.
Qualify reuse through the current durable policy version and exact payload;
legacy versions without that qualification need a deliberate migration boundary.
Preserve policy ABA and direct-override invalidation rather than treating equal
payloads alone as unchanged permission. The eight-offer reopen, legacy-marker
and publication-rollback controls in the
[consent owner](../crates/mesh-talk-core/src/node/file_consent_store/indexed.rs)
exercise this boundary.
Match cache persistence to its actual lifetime. Runtime-only hints should not
pay durable WAL commits when restart deliberately invalidates their evidence.
Keep volatile metadata bounded, preserve current-owner checks and prevent cache
hits from becoming custody. The runtime-hint regressions in the
[opaque receive owner](../crates/mesh-talk-core/src/node/opaque_receive_cache.rs)
deny persistent hint writes and exercise TEMP rollback, retry and configuration
failure while retained evidence remains independently durable.
Nonempty retry queues can also be unchanged: rewriting the numeric tickets of
an already selected global suffix does not advance fairness. Prove the exact
account-wide order with a bounded metadata read before skipping rotation;
matching only the maximum ticket misses interleaved conversations. Keep byte
admission, completed-row deletion and fresh ownership checks. The idle-tail and
interleaved-conversation regressions in the
[file receive owner](../crates/mesh-talk-core/src/node/filebook/indexed.rs)
measure actual SQL changes and committed writes while preserving new-work turns.
Apply this rule at shared lower owners too: terminal export polling must retain
current selection/authority checks without republishing the same cursor for each
ID page. The direct and mailbox consumers share that rule in
[selected export](../crates/mesh-talk-core/src/eventlog/repository/selection/export.rs).
When measuring completed work, first assert its durable queue lane is absent;
an existing lane can make insertion idempotent while hiding recreate/retire
churn. The settled account-maintenance regression exercises actual reception
drain before repeating maintenance and checking changed-source recovery.

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
The `quiet_file_consent_rotating_iteration_has_bounded_checkpoint_writes` and
`rotating_offer_checkpoint_*` regressions in the
[file-consent store](../crates/mesh-talk-core/src/node/file_consent_store/indexed.rs)
check real multi-page rotation, restart progress, checkpoint failure, competing
writers and bounded cursor eviction.
The [group-file retry regressions](../crates/mesh-talk-core/src/node/group_file_authorization_tests.rs)
cover missing routes, cancellation, failed refill, full capacity and unfinished
discovery alongside cooled jobs. The private-file scheduler regressions in
[delivery runtime](../crates/mesh-talk-core/src/node/delivery_runtime.rs) and the
shared [retry owner](../crates/mesh-talk-core/src/node/delivery_retry.rs) cover
unrelated wakes, cancelled reservations, replacement attempts, completed-step
progress and capacity churn without retaining file payloads in cooldown hints.

A scheduling readiness predicate must represent completed negative results as
well as materialized positive output. A completely projected, current-source
channel can prove that the local device never joined and therefore intentionally
has no presentation row. Requiring that row recreates demand on every unchanged
observation. Reuse the existing account/source/full-count evidence at the
[metadata demand owner](../crates/mesh-talk-core/src/node/channel/checkpoint/presentation.rs);
keep current membership and plaintext admission at their original consumers.
The nonlocal-demand regressions in the
[checkpoint tests](../crates/mesh-talk-core/src/node/channel/checkpoint/tests.rs)
cover unchanged read-only demand, a later signed membership, stale sources and
retired participant generations. Absence of a current row or a partially prepared
reducer is not equivalent negative evidence. A never-created source can be
quiet only after checking that no pending, projected or retirement owner needs
work. Keep this absence check off the ordinary prepared-source path; recheck it
on each demand and retain the normal committed-source discovery recovery.

## R06: Exact authority and stale completions

When projecting account roles into device-shaped display fields, derive the
representative from the current validated account membership. A historical
legacy device owner cannot describe a transferred account role. Keep exact
account/device validation and bounded-snapshot inclusion together at the
[inventory publisher](../crates/mesh-talk-core/src/node/account_documents/inventory.rs);
the real modern and legacy transfer regressions in
[owner target tests](../crates/mesh-talk-core/src/node/account_group/tests/owner_targets.rs)
check readable inventory while preserving grants and the legacy owner field.

**Failure pattern:** work authorized before an await or queue delay commits after
logout, same-user relogin, account replacement or a policy off/on transition.
Comparing only the username or current Boolean value misses changed authority.

Capture the owner's exact session/runtime installation and relevant revision at
entry. Revalidate through the existing atomic commit seam before side effects
and before publishing private results. Treat frontend generation guards as UI
ownership, not host authorization. Read commands can expose stale private data too.
Bind session effects to exact owner/generation rather than a replaceable display
object. Cosmetic profile refreshes must not restart inactivity timers, republish
call state or count as user input. The mounted [App regressions](../frontend/src/App.test.ts)
exercise metadata updates, actual input and same-owner session replacement.

A closing popup's delayed cleanup can outlive a newer popup's focus ownership.
Default restoration may act only while focus is neutral or still belongs to the
closing content. Reuse the [shared focus owner](module-boundaries.md#dialog-focus-ownership)
for dialogs and context menus; preserve consumer cancellation and ordinary trigger
restoration. The real Radix controls hold old unmount cleanup until a newer
popover owns focus, then require that popover and its action to remain usable.

A routing or capability preview must not become a second admission owner. When
the canonical reader reports a typed stale projection, preserve its existing
bounded preparation path before deciding authorization. Any cached local candidate
still needs full identity and current authority checks at the real commit seam.
The [forwarding owner](message-forwarding.md#source-admission-and-lifecycle) tests a
signed rename that needs preparation, plus dissolution and same-user/different-key
controls that must never produce a message or send receipt.

An asynchronous prepare/commit API must revisit mutable prerequisites after
expensive staging yields. A later synchronous commit allowance cannot finish an
arbitrary intervening control tail. Recheck cheap expiry and caller admission
before resuming the existing bounded preparation owner; check expiry again after
that await and preserve final atomic authorization. The
[channel file staging regression](../crates/mesh-talk-core/src/node/files.rs)
commits a real signed rename tail during chunk progress and requires exactly one
manifest; the sidecar recovery regression retains ordinary/protected acceptance
and rejects any authorization after its durable WAL commit.

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
Durable local erasure must classify its target from canonical identity, not an
enumeration of display projections. A ready history observation is not a lease
on subsequent projection readiness, and a generic mutation retry can repeat work
that already committed. The [local erasure owner](../crates/mesh-talk-core/src/node/lifecycle.rs)
checks a real pending archived projection, unrelated DM/unknown targets,
suppression failure without erasure, and retained suppression after reopen.
Do not attach unrelated global projection work to a narrow mutation. Text
erasure has no file keys to revoke; a full file-book reconciliation can fail on
unrelated reads or concurrent acceptance after suppression already committed.
The `owned_read_failure_text_erasure_does_not_reconcile_unrelated_files`
regression in [delivery tests](../crates/mesh-talk-core/src/node/delivery_runtime_tests.rs)
qualifies the real read failure, verifies text removal and suppression, and
retains the unrelated live file. File erasure still revokes its keys before
physical removal, and reconciliation still rejects stale revisions.

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

Separate authoritative durability from disposable SQL query progress. The
[selection owner](../crates/mesh-talk-core/src/eventlog/repository/selection.rs)
uses the existing connection's encrypted temporary pager for derived ranks and
cursors; reopening rebuilds them from current MAIN records. Failed preparation
cannot become a receipt, custody or permission. Preserve MAIN transaction
and replay guarantees while measuring its actual writes independently of scratch
mutations. The `disposable_selection_prepare_export_and_drop_leave_main_storage_unchanged`
regression checks selected/exported results and MAIN write evidence together.

Distinguish filesystem publication durability from later database mutations.
The [history source publisher](../crates/mesh-talk-core/src/node/transcript_repository/publication.rs)
synchronizes the barrier file and, where supported, its renamed parent directory
before SQL marks a source published. Retrying a failed initial sync must finish that ordering without
replaying a committed import. Once published, every write still validates the exact
barrier, while SQL FULL transactions own row durability; unchanged directory entries
must not be synchronized for each row mutation. The publisher's
`initial_history_barrier_sync_failure_requires_retry_before_publication` regression
and the opaque checkpoint owner's
`changed_cache_sql_writes_do_not_republish_barrier` and
`failed_checkpoint_sql_write_preserves_previous_value_and_retries_after_reopen`
regressions retain initial-sync retry, barrier rejection and SQL rollback/reopen
evidence independently of the directory-sync budget.

Scope destructive invalidation to actual dependencies while retaining a global
fence for cross-conversation authority. The
[repository classification owner](../crates/mesh-talk-core/src/eventlog/repository/selection/metadata.rs)
subtracts only atomically classified, unreferenced Profile leaf compaction from
that global fence; each affected conversation still invalidates its own physical
epoch and exact history binding. Do not exempt raw appends, real parent retirement,
erasure or unknown/corrupt classification. The
[participant network regressions](../crates/mesh-talk-core/src/node/privacy_sync/network.rs)
check unchanged dependent file exports and retained manifest/proof/ancestor facts,
own-view rejection, classifier rollback/retry, corrupt or missing metadata, and
logical parent retirement before physical deletion.

Version the private hash domain when a persisted derived fingerprint changes
meaning, even if its serialized fields and wire layout remain identical. Changing
from a full destructive counter to a classified counter can otherwise let an old
completion proof regain validity after a real retirement reuses its numeric value.
The participant network
`participant_history_sync_stamp_never_aliases_legacy_full_counter_after_parent_retirement`
regression reconstructs the exact legacy serializer and exercises that collision
through actual compaction and parent retirement. Invalidate only the obsolete
completion qualification; preserve compatible receive cursors and pending work.

Give finite buffer dispatch an explicit settled state. A remote buffer's
capacity eviction or replacement must not silently become perpetual receiver
demand for all retained source history. The
[group-file owner](../crates/mesh-talk-core/src/node/group_file_delivery.rs)
records actual accepted dispatch against the retained offer, full recipient and
exact receiver action; the file-book foreign key removes scheduling metadata with
that offer. A committed new request survives an older upload's completion, and a
failed settlement commit retains retry. The real mailbox publisher regression
covers actual HAVE, failed persistence, relay replacement, a new signed request,
duplicate requests, the completion race and independent incoming traversal.
Qualify receiver actions per file: an unrelated account policy generation must
not manufacture new demand, while current policy still fences publication.
The same distinction applies to send-only historical publication: remote buffer
retirement cannot rewind an unchanged source dispatch. The
[mailbox owner](../crates/mesh-talk-core/src/node/opaque_relay/incremental.rs)
preserves duplex receive reset and repair, while explicit targets require fresh
durable HAVE and current selected export before a missing target is republished.
The real replacement-store regression covers a cold dispatch owner, selective
target recovery, transport failure and retry, and changed local source input.
Keep genuine file requests distinct from archival publication: the file owner
must redispatch every signed chunk, under a separately qualified send-only key.
Temporary ready confirmation views retain their admission lease through
cancellation cleanup; partial preparation keeps its existing protection. The
four-scope cancellation regression requires real unrelated admission before the
ordinary unfinished-work timeout, without increasing the view budget.

Keep UI observations distinct from durable progress: phase/check timestamps
must not rewrite a reception ledger. An unchanged committed cursor can reuse its
successful commit only while the exact owner has no dirty durable state; failed
saves must retain that dirtiness and block ACK. The
[reception owner](../frontend/src/store/chat.ts) and its chat regressions cover
observation-only wakes, epoch changes, batch progress and quota-failure retry.

An unchanged deferred revision must retain its failure history across worker
admission, with capped retry spacing and a real dependency wake. Identical
observations must not reset cooldowns; a newer committed revision or dependency
change during active work must survive its older completion. Bound retry metadata
to pending/active work and exclude active deadlines from timer selection. The
[account fanout owner](../crates/mesh-talk-core/src/node/account_sync/fanout.rs)
checks capped spacing, revision replacement, wake races, overflow cleanup and
success deduplication; the real source-invalidation fanout regressions retain
original event/sequence authority through repeated failure and recovery.

A validated durable fallback must warm the existing bounded runtime owner even
when no subsequent mutation occurs; otherwise fresh adapters can repeatedly
verify unchanged evidence. Normalize runtime binding only after rejecting retired
session-only evidence, and preserve cold repair reset and actual lease ownership.
A busy adapter does not exclude maintenance: compare its captured receipt under
the same owner guard used to publish warming, so a concurrent repair rewind
cannot be restored by stale initialization. Equality of two empty values does not
prove no invalidation occurred: capture and recheck the owner's reset epoch too.
Optional work evidence must record under the same invalidation order and decline
when its actual lease or captured generation was retired.
The [mailbox checkpoint regressions](../crates/mesh-talk-core/src/node/opaque_relay/incremental/checkpoints.rs)
cover fresh adapters without signature reopens, SQL commits or directory syncs,
retired-runtime rejection and repair progress under its live owner.

Classify retained custody, durable semantic processing and session-only handling
separately. A successful immutable identity import or qualified monotonic control
fact can preserve receive progress without retaining its raw event. Keep account
and repair fences, conservative mixed/failed classifications and legacy exchange
progress; semantic evidence must never enter HAVE or custody. The
[incremental receive regressions](../crates/mesh-talk-core/src/node/opaque_relay/incremental_receive_tests.rs)
retain the actual checkpoint through reopen instead of deleting it before testing.

Keep retry scheduling distinct from durable progress. A bounded volatile order
may replace repeated ticket writes, but must reconcile current pending rows,
retain oversized attempts and publish only after the database operation succeeds.
The [file receive queue](../crates/mesh-talk-core/src/node/filebook/receiving.rs)
uses a shared bounded runtime owner with indexed durable fallback. Its inline
file-book regressions cover blocked cross-conversation fairness without commits,
capacity shrink, failed cleanup, stale owners and oversized/reopen recovery.

Profile receive uses immutable durable admission tickets with independent bounded
runtime cursors for scoped callbacks and global recovery. A blocked nonempty
queue must rotate fairly with zero SQL changes, write commits or dirty MAIN pages;
reopening can repeat checks but cannot lose pending/processed facts. The owning
`blocked_profile_retry_rotation_is_read_only_and_reopen_preserves_pending`
regression checks those costs rather than only the empty-queue case.

A cleanup cursor is not a deletion receipt. Checkpoint one bounded page rather
than every protected-file read; retain fresh references, exact revisions and
durability for each real deletion. On failure/reopen, recheck the bounded page
without skipping unresolved entries. The
[media owner](../crates/mesh-talk-core/src/node/media_store.rs) tests a full
protected page's commit count and safe replay through a fresh profile owner.

A profile-wide physical write stamp is a conservative invalidator, not a semantic
revision: disposable query preparation can otherwise invalidate its own work
hints. Exclude derived writes only inside the connection owner's guarded table
boundary. Observe prior ordinary writes before the same-lock derived interval;
permit TEMP scratch but reject business-table writes through direct SQL, cached
statements or triggers, and restore default authority on success, failure and unwinding. Schema and
source/authority indexes keep conservative invalidation. The database work-clock
regressions and the real incremental-mailbox consent exchange cover independent
selection preparation/retirement, transport checkpoint advancement, actual
authority invalidation and rollback. Optional transport hints have their own
closed table set; they cannot acquire selection or business-write authority.

A directed lookup must select its target before hydrating every cached peer or
consulting unrelated group authority. Preserve live-roster precedence and the
existing cached-route order; recheck the selected target's policy and keep dial
admission independent. Drop the roster guard before entering the cached-policy
fallback. The [routing owner](../crates/mesh-talk-core/src/node/privacy_routing.rs)
regressions hold unrelated owners to check lookup isolation, then verify target
revocation, cached ordering and missing results.
Progress in one mailbox must not invalidate every settled consent offer merely
because both happen to share the same encrypted database.

Treat derived-index cache retirement as a storage mutation when its destructor
marks/deletes SQL rows. Fully version-qualified plans must not expire only because
no request used them recently; enforce capacity and owner/authority invalidation
instead. The encrypted paged-request idle-gap regression measures actual changes
and commits, including across new connections. Cache an absence only after a
complete current selection proves it; errors, unavailable payloads and unknown
versions must remain retryable. Six-mailbox confirmation rotation checks zero warm
SQL work and recovery when the previously absent event arrives.

Preserve accepted intent when presentation fails. Report what actually committed;
do not relabel a committed change as rejected because later best-effort work
failed. Avoid empty catches, blanket defaults or unconditional retries that hide
the distinction. Follow the owning feature guide for rollback and durability.

When bounded discovery and compaction share a source, discovery may not have
visited an item before its raw bytes are removed. Record the exact authorized
retirement fact with deletion in the same transaction; a volatile cache or a
later scan cannot supply that evidence. Keep source-account attribution separate
from the current account and from presentation or custody. The rollback, adoption
and undiscovered-profile regressions in the
[profile receive owner](../crates/mesh-talk-core/src/storage/profile/profile_receive.rs)
exercise this boundary.

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

Check read granularity as well as file size: streaming JSON parsers can turn
each byte into an OS read. Use bounded buffering for files, as required by the
[serde_json reader contract](https://docs.rs/serde_json/latest/serde_json/fn.from_reader.html),
while preserving fresh reads and authority checks. The
[contact policy tests](../src-tauri/src/contact_policy.rs) own size, corrupt-file,
failed-read and persistence semantics; native I/O traces qualify the read-call cost.

Remote rejection does not itself invalidate qualified local preparation. An
existing bounded cache may retain it after a completed protocol outcome, with
current source and authority checks on reuse; this never certifies remote
acceptance. Best-effort hint maintenance must not replace the original failure.
The [paged-session regressions](../crates/mesh-talk-core/src/node/session/paged_tests.rs)
own rejection, source/privacy invalidation and transport/cancellation cleanup.
Local selection membership and remote custody have different lifetimes: relay
replacement must not discard valid local target preparation, and local reuse
must never substitute for a fresh remote custody probe. The
[incremental mailbox tests](../crates/mesh-talk-core/src/node/opaque_relay/incremental_tests.rs)
repeat selected and absent targets after eviction, replacement and invalidation.

A bounded page is still expensive if an unchanged timer keeps rescanning it.
Gate repair scans with pre-work semantic input, retain short continuation until
a complete fair sweep, then park until a relevant event or sparse fallback.
Independent expiry/recheck clocks must not admit the full scan. Actual-owner
quiet, continuation and recovery controls live in the
[channel custody tests](../crates/mesh-talk-core/src/node/channels.rs).

Separate known-peer lease renewal from unknown-host discovery. Sparse subnet
recovery must not expire established peers, miss visibility restoration or truncate
a large known-target tail. Gate hidden producers before OS inventory work and
retain per-send disclosure checks. Real UDP and target-budget controls belong to
the [discovery owner](../crates/mesh-talk-core/src/discovery/service.rs).

Trace every consumer of a scheduling projection: relay-control retries, file
continuations, profile publication, account access and receive maintenance must
share canonical endpoint semantics. Equal projections preserve owners, completion
and backoff; new attempts still read current transport choices. Raw observations
continue to refresh liveness. The
[delivery retry tests](../crates/mesh-talk-core/src/node/delivery_runtime.rs),
[publication tests](../crates/mesh-talk-core/src/node/runtime_routed_tests.rs) and
[account-access tests](../crates/mesh-talk-core/src/node/account_access.rs) own
these consumer boundaries.

Coalescing must preserve semantic scope as well as queue size. Reusing a value
must not claim a new read observation or suppress a pending result for another
resource; retain independent publication order for peers and account metadata.
Retain bounded invalidation domains through pending-page ACKs; do not turn a roster pulse,
progress update or empty read into a global refresh. The native reception gate
and registered IPC tests, together with the [chat consumer](../frontend/src/store/chat.test.ts),
exercise quiet pages and changes arriving during acknowledgement.

Filter lightweight metadata before cloning large payloads. An empty final result
does not prove that a query avoided full-record reads. Persistent indexes of
authoritative records must migrate and update in the same transaction as those
records; failed commits cannot acknowledge work. Disposable query indexes retain
bounded row turns and admission while spilling through the encrypted temporary
pager, rather than collecting history in Rust. Its 8192 KiB page-cache setting is
a target, not a process-memory cap. The
[query pager controls](../crates/mesh-talk-core/src/storage/database/query_scratch.rs)
exercise spill encryption, rollback and restoration of TEMP storage before hint
initialization; connection close discards plans without discarding source facts. The customization storage regression in
[packs tests](../frontend/src/store/packs.test.ts) and real
[IndexedDB browser scenario](../frontend/e2e/customization-pending-storage.spec.ts)
checks empty pending reads, old-data migration and failed writes.

Read-only retry selection must not renumber a queue that already fits one turn.
Retain authority checks and exact-revision completion; rotate durably only when
contention changes fair order. The [filebook regressions](../crates/mesh-talk-core/src/node/filebook/indexed.rs)
assert zero changed rows and write commits for all-fit claims, plus ninth-item
progress and atomic rollback under contention.

Closed UI is not demand loading if its static imports still initialize documents
or inactive feature trees. Separate lightweight navigation from payload producers.
Keep the dialog/controller owner stable while scoped imports settle or fail; test
close, owner replacement, retry and eventual control focus. The [help reader](../frontend/src/features/help/HelpDialog.test.ts)
and [settings page](../frontend/src/features/chat/settings/SettingsPage.test.ts)
regressions exercise these lifetimes without weakening mutation ownership.
A metadata catalog must exclude image/archive payloads, including snapshots held
by long-lived subscriptions and registration receipts. Shared hydration qualifies
each reader before queued disk admission, and a canceled observer cannot release
an admitted worker slot. The [pack demand regressions](../frontend/src/lib/packDemand.test.ts)
cover queued retirement, surviving shared demand and finite admission.

A bounded work item can still be unnecessary work. Classify demand from the
owning source before constructing a derived index; stale queue entries alone
must not classify its domain. Preserve later valid demand while retiring stale
work. The ordinary-DM and delayed-membership regressions in
[group metadata tests](../crates/mesh-talk-core/src/node/group_metadata_tests.rs)
check that channel recovery does not prepare ordinary private conversations.

Do not regenerate an immutable signed self-binding on every group query or in an
eager fallback expression. The Node lifetime owns one proof derived only from
its fixed device/account keys; account adoption creates a new owner. Initializing
that proof must not acquire group, privacy or log owners. Current authority checks
still run on every use. The [account-group access regression](../crates/mesh-talk-core/src/node/account_group/access.rs)
counts actual proof construction and checks isolation across different accounts
using the same device.

Separate background work classes when one class's continuation can restart
another class's failed work. A shared notification or retry clock can defeat
otherwise correct backoff. Keep each producer's retained wake, meaningful
invalidation subscriptions and cancellation owner explicit. The
`isolated_file_lane_` regressions check that outgoing queue admission neither
wakes receive repair nor shortens its sparse deadline.

Batch limits do not bound repeated expensive validation within each row. Trace
the complete consumer path, including helpers that repeat another public query.
Request-local reuse may retain intrinsic certificate verification under a bounded
complete cryptographic key; it must not retain mutable membership, holder bindings,
document availability or final counts. Verify actual crypto invocation counts and
mutate authority or stored observations between rows. The custody batch regressions
in [node tests](../crates/mesh-talk-core/src/node/node_tests.rs) cover this boundary.
Trace the actual read owner's guarantee before sharing a verified object:
encrypted legacy replay need not verify signatures as indexed reads do. Exact
equality with an owner-validated durable record can reuse intrinsic verification,
but must preserve current erasure checks and failed durability retries. The
[group-file controls](../crates/mesh-talk-core/src/node/group_file_authorization_tests.rs)
count real body reads and signatures, retain nonempty signed decisions and test
read failure/recovery. Return the already-authenticated decoded value to callers
instead of discarding it and reopening its signed event; the warm terminal
recipient regression counts real consent signature calls across repeated queue
reconstruction. [Delivery-store controls](../crates/mesh-talk-core/src/node/delivery_store_tests.rs)
cover identical, conflicting, forged, reopened and erased reservations.
Hot point reads should reuse bounded compiled statements without retaining
results or weakening source checks. The [event repository regressions](../crates/mesh-talk-core/src/eventlog/repository/tests.rs)
count actual VM executions for repeated hit/miss reads and keep domain binding,
new appends and schema failure/recovery observable. Body authentication and
retirement checks still run after compilation reuse.

Intrinsic signature memos must key every verification input and retain no body
or permission. Current reads, canonical/content checks and source errors still
precede reuse. The [repository regressions](../crates/mesh-talk-core/src/eventlog/repository/tests.rs)
cover unchanged reads, forged signatures, repeated negative results, content
tampering, retirement, reopen and bounded eviction.
A selected peer-directory record may reuse the exact proof verified at admission
or authenticated replay; recheck current linked-account bindings before returning
an unchanged result. The peer-directory regression
`identical_selected_proofs_reuse_verification_but_keep_linked_binding_checks`
counts certificate verification and covers changed bytes, linking and reopen.
A live roster may reuse its already-verified announcement only on full value
identity, including both public keys, every advertised field, certificate and
signature. Keep current account bindings, route scope and presence updates
outside that reuse; forgotten/evicted announcements lose the evidence. The
[roster regressions](../crates/mesh-talk-core/src/discovery/roster.rs) count actual
certificate checks and exercise tampering, renamed proofs, binding transitions
and live-proof retirement without adding a separate cache.

Separate cache admission priorities when recomputable work can displace receive
progress and repeatedly restart unchanged synchronization. Preserve shared hard
budgets and plan eviction before mutation when a class may refuse admission. The
[checkpoint pressure regressions](../crates/mesh-talk-core/src/node/opaque_receive_cache/checkpoints.rs)
check receiver retention and atomic refusal under count/byte pressure; the signed
cold-receipt loopback regression checks recovery without new writes. Reject
impossible optional cache admission before entering a guarded write transaction:
changing a SQLite authorizer expires prepared queries even when no rows change.
Derived TEMP receive hints must also use the existing closed transport-hint
transaction owner: ordinary `total_changes` notifications would wake the source
consumer for its own cache updates. Read exact unchanged, bounded pages before
installing that authorizer. The
[receive-cache regressions](../crates/mesh-talk-core/src/node/opaque_receive_cache.rs)
check source-watch silence, real append/retirement wakes, session recovery and
unchanged cached-statement plans.
Keep the actual mutation guard and capacity recheck; a racing capacity release
may miss an optional hint until retry, never authorize eviction. The byte-pressure
regression checks zero `RePrepare` after both refused insertion and refused growth,
unchanged rows/accounting, and successful admission after capacity is released.
Eviction of a large work owner is not a source change. When losing its small
completed-work facts restarts an unchanged scan, retain bounded scheduling
fingerprints independently of the heavy data. Bind reuse to the exact authenticated
state, runtime and policy; keep authority checks at use, cap retained metadata and
preserve a nonrenewable repair deadline. Restart, corruption and missing evidence
must recover normally. The [mailbox receipt regression](../crates/mesh-talk-core/src/node/opaque_relay/incremental/checkpoints.rs)
`completed_repair_keeps_quiet_work_across_bounded_owner_eviction` exercises actual
fixed-capacity admission after a completed repair; the adjacent fingerprint tests
cover changed inputs, capacity and the original deadline. This evidence never
restores a compaction lease, cached parent body or permission.

Apply the same rule to terminal preparation: check the live selection, source
and actual export cursor before entering a write guard. The
`ready_history_upload_id_pages_do_not_rewrite_terminal_export_progress` regression
checks zero `RePrepare` for both upload and direct-network consumers, alongside
zero commits/body reads and rejection after authority changes. A no-op SQL UPDATE
is insufficient when installing its guard invalidates unrelated read plans.

Reuse compiled static SQL through the existing bounded connection cache rather
than caching authorization results. Every execution must still bind the current
parameters and perform the original source, account and lifetime checks. Warming
a statement must not freeze a read snapshot, retain old bindings or suppress an
absence or storage error. Exercise changed parameters and retired owners after
warming the actual production queries. The transcript
`warm_metadata_reads_reuse_statements_and_observe_current_rows` regression checks
actual VM reuse, read-only metadata, changed bindings, append/deletion and schema
recovery without loading bodies. Intrinsic document memos similarly exclude
receipt-derived publication authority even when the signed payload is unchanged.

Within one connection transaction, reuse a fresh observation through its owning
role helpers instead of querying the same row again. Retain validation order,
owner/lifecycle/cutoff checks and storage failures; do not extend the observation
past the guard or across an await. The post-office
`network_validation_reads_arrival_once_without_weakening_current_checks`
regression counts actual SQL executions for absent and populated mailboxes and
checks current source failure/recovery and changed-arrival invalidation.

Suppress unchanged producers before entering those verified reads. A publication
gate must cover every effective input owner, retain the selected pre-work stamp
through bounded pages and leave newer changes dirty. Sparse integrity repair and
owned failure deadlines remain necessary; a failed pass is not completion.
The [local publication regressions](../crates/mesh-talk-core/src/node/account_sync/local_maintenance.rs)
count actual body reads and cover profile/proof/control changes, in-flight
invalidation, failed writes and timer-driven recovery without periodic peer polls.
Comparing unchanged publication after loading its encrypted old payload is still
repeated work. Delivery retries must evaluate the durable reference and current
retry deadlines before hydrating the event body. The real direct-custody
regression in [delivery runtime tests](../crates/mesh-talk-core/src/node/delivery_runtime_tests.rs)
observes actual retry completion, then requires unchanged repository body-read
counters while custody is parked; new demand and real recipient receipts remain
prompt. Ordinary/legacy operation publishers qualify at most 128 runtime
metadata/projection pairs under the exact verified own-head hash, with a
nonrenewable five-minute repair deadline. Settled ordinary Text/Sticker/File
results can qualify before body materialization only with complete current journal,
source/destructive and File row/suppression/recall metadata, observed unchanged
before and after materialization. Pending/query/forwarding decisions and unsupported
Recall or expiring File inputs retain live projection. Both absolute and first-seen
expiry require live evaluation. Changed heads, missing verification and sparse
repair still re-read. The [operation publication regressions](../crates/mesh-talk-core/src/node/account_documents/group_operations.rs)
count actual EventRepository body fetches/decodes for real committed Text and File
results as well as document reads and signatures; a Rename-only fixture or a
document-store counter cannot detect repeated event preparation. They also cover
changed inputs, File retirement, expiry, repair and replacement.
Explicit document reads continue to authenticate durable bytes; this scheduling
qualification never serves a payload or grants permission.

Callback-to-stream
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
Project private API results into explicitly allowed fixed labels or bounded
scalar counts before logging, including failure-only test output. Do not
use `Debug` to log whole tuples or containers containing identifiers. The
[runtime restart control](../crates/mesh-talk-core/src/node/runtime_privacy_tests.rs)
retains delivery evidence without logging its private observation tuples.

## R10: Evidence that exercises the failure

For browser compatibility changes, inspect the actual embedded engine's feature
support and resolved production CSS. A JavaScript target or installed browser
version does not prove CSS compatibility. A fallback that loses alpha can make
a same-color SVG disappear even though its paths render correctly. The
[production frontend owner](testing-feedback-loop.md#production-frontend-startup)
and [theme alpha regression](../frontend/scripts/theme-alpha.test.mjs) preserve
HSL transparency through the real CSS pipeline; native light/dark computed-style
and visual checks remain separate acceptance evidence.

A successful package-index command can still report failed repositories and use
cached indexes. Release preparation must reuse the
[hosted Ubuntu source owner](../scripts/ci/ubuntu-apt-source.mjs), reject any index
refresh error before installing dependencies, and preserve repository signing and
architecture. Any download-mirror change belongs in that owner, with exact origin
validation and separate normal/ports paths; it must not replace Ubuntu's signing
authority or disable freshness checks. The [release workflow regression](../scripts/diagnostics/release-workflow.test.mjs)
executes its shell boundaries with preparation/index failures and proves that
installation does not follow. Fixture success does not establish runner DNS or
signed hosted release acceptance.

Dependency upgrades can change equivalent CSS serialization without changing the
interaction contract. Keep assertions narrow enough to reject wrong geometry and
scale while accepting equivalent omitted zero coordinates. The reset assertion in
[media preview tests](../frontend/src/features/chat/MediaPreview.test.ts) accepts
one-axis or two-axis zero translation while preserving the exact wheel-anchor,
drag-clamp and cancellation checks; reproduce the old assertion failure against
the upgraded DOM implementation before changing it.

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

Cached certified discovery is not completion of a newly issued query. The
[privacy fixture](../crates/mesh-talk-core/src/node/runtime_privacy_tests.rs) requires
this invocation's nonce, exact endpoint and actually accepted device/account
binding before releasing later setup. A real stopped target with a warm cache
must fail; restart recovery must obtain a fresh response with the same identity.
Instance-owned, opt-in [discovery witnesses](../crates/mesh-talk-core/src/discovery/service.rs)
retain bounded public metadata with explicit omissions. Qualify real UDP
acceptance/rejection, disabled outcomes and listener retirement; traces do not
establish a historical failure stage that was never observed.

An explicit loopback exchange can accidentally share its UDP send capacity with
environment-dependent scans. Isolate those fixture producers through named task
ownership, preserving the actual listener and protocol admission. The
[participant owner](../crates/mesh-talk-core/src/node/participant_runtime.rs) aborts
all selected tasks before joining any and retains every join failure until cleanup
finishes. Per-instance first-poll and actual-join controls distinguish isolated
startup from ordinary ambient startup. This does not prove the cause of a past
send timeout or qualify production behavior under network pressure.

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
Its initial history preparation observes exact identity, text and visible geometry
inside the browser with direct mutation checks and a frame fallback. Absent and
hidden rows must expire, and queued recovery with frames held must retire every
observer and callback. This qualifies readiness observation, not the cause of a
previous rendering delay.

Keyboard focus evidence must inspect resolved style, width and theme-token color,
not utility class names: class merging can retain `outline-style: none`. Wait
within the existing assertion budget for declared color transitions to settle.
Shared audit helpers must make teardown explicit when a later assertion still
needs the open menu; preserve default cleanup for other callers. The
[focus owner](module-boundaries.md#dialog-focus-ownership) retains real keyboard
navigation and activation controls in both themes.
Visibility during an exit animation does not certify an open popup. Check the
semantic open state and complete the new action; observing the transient shell
alone can accept an already dismissed interaction.

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
Historical scope completion does not prove an existing child or pair has retired.
When a fixture requires a natural unpaired read, establish it through receiver-owned
demand before publishing the Wake that deliberately creates its replacement; observe
the exact setup task, page completion and pair retirement before arming the next hold.

Concurrent actual-tool fixtures must own output directories, not only source
and temporary data. A second Playwright invocation can otherwise clean a running
suite's traces. The browser command owner retains live logs and case progress,
joins descendants before reporting and fails on timeout or uncertain retirement.

Keep a test's observation scope equal to its named requirement. A live
transport-authorization fixture must first qualify actual authentication and
clean channel reuse outside the measured attempt, then change authority only
after the measured mailbox callback is admitted. A pre-dial check does not
prove a live exchange or a missing post-revocation check. The
[group file authorization fixture](../crates/mesh-talk-core/src/node/group_file_authorization_tests.rs)
retains its production and measured watchdogs, requires multiple post-authentication
checks and labels the failing mutation and phase. If reuse is a setup requirement,
a fixture must authenticate the same signed role it advertises in the roster;
identity-only Noise cannot stand in for an authenticated post-office role.
Keep the matching presence, actual pool checkout and owned server retirement
rather than weakening the production role-binding check.
The
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
control derives the actual source configuration and accelerates only the
deliberately parked case. Later cases keep the normal source allowance; inject
latency beyond the accelerated bound to verify that isolation. Require timeout
failure plus later-case execution with joined descendants; never disable the
watchdog or use retries to qualify a failure.

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
Install response holds before admitting background producers, and observe the
actual held handler before publishing the state that must remain unavailable.
Arming after launch cannot stop an earlier request that already passed the hook
from reading later state. The exact-peer hostname hold in
`channel_custody_converges_while_directory_is_held` in
[account document tests](../crates/mesh-talk-core/src/node/account_documents/tests.rs)
preserves that ordering while measuring independent custody progress.

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
in `runtime_retirement_tests.rs` owns this observer regression. Install bounded
readiness and task-origin observers before starting producers: unchanged input
may never repeat an already completed transition. Arm blocking test gates after
bootstrap so observation cannot create a setup dependency cycle. The selected-source
and queued-source controls in `node/account_group/tests/` retain the real page,
live-child and settlement predicates rather than substituting aggregate status.
An inbound-only source cannot converge a bidirectional union digest through
one-way reads. Keep the required receiver owners live during preparation, or
finish symmetric repair after all initial publications before measuring. Bind
partial-page proof to the measured publication, exact live request and validated
completion; startup pages cannot satisfy a later phase's continuation condition.

Negative idle assertions must consume startup JoinSet results and retained wakes,
then invalidate the old idle marker when the target event is observed. Await the
owner's new idle boundary before counting admitted work; a retired worker or one
cooperative poll cannot certify that its parent handled the event. The
[account maintenance owner tests](../crates/mesh-talk-core/src/node/account_sync/local_maintenance.rs)
exercise the actual roster observation and post-event scheduling boundary.

A periodic probe can miss readiness between its last check and the deadline.
Use [R05](#r05-bounded-work-must-also-make-progress)'s waiter protocol to recheck
the actual condition on a transition, retaining the original deadline and bounded
fallback. A snapshot taken after timeout is diagnostic evidence, not success.
Screenshot dismissal acceptance uses a browser-local MutationObserver under its
original five-second monotonic deadline, instead of driver-side count polling.
The [screenshot confirmation observer](../frontend/e2e/helpers/screenshot-confirmation.ts)
records actual timely DOM commits even when the driver consumes the result later.
Its real-browser controls in [screenshot scenarios](../frontend/e2e/screenshot-portable.spec.ts)
reject missing and late transitions, retain cleanup checks and preserve the crop,
clipboard and staging assertions. The canonical browser inventory selects this
control alongside both Retina viewport journeys.
An isolated DOM control retires its observers and timers before restoring real
application content for shared screenshot capture. A passing test on a blank
document is not meaningful visual evidence; retain the strict PNG rejection.
A manual-recovery fixture must keep its provider failure active until the real
recovery gesture. One failed call does not hold an error state while automatic
timer or focus retries remain live. The [unread badge scenario](../frontend/e2e/unread-badge.spec.ts)
keeps those retries enabled, rejects synthetic recovery and requires the trusted
retry to write the current cleared count before retiring its controlled fault.

The metadata-hold and repair-body-hold controls in the
[opaque mailbox regressions](../crates/mesh-talk-core/src/node/opaque_relay/incremental_receive_tests.rs)
qualify separate setup and admitted repair cancellation: setup cannot consume the
progress budget, and that budget must still cancel actual owned work.

A storage fault fixture must fail the currently selected persistence owner and
observe that failure before asserting recovery. A hook on a neighboring journal
cannot certify the actual commit failed merely because both use the same logger.

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

Desktop archive fixtures must use the same account-owned channel genesis as the
desktop creation command and assert its owner before seeding content. Legacy
device-only genesis starts a separately consented background authority migration;
it cannot stand in for the desktop factory. The
[cloud archive command regression](../src-tauri/src/google_drive/command_tests.rs)
owns this production-path precondition and preserves real file/archive/restart
acceptance; core channel-maintenance tests own legacy migration.

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
The queued A-side paired hold must bind the scheduler's actual `paid_resume`
dispatch to its exact live child task. Same-source foreground replacements can also carry
the resumed/protected trace labels; those labels cannot certify a paid parked
turn or trigger its before-B ordering assertion. Retain the classifier's negative
foreground control and the real queued Noise scenarios under core-account-group
validation. A cross-peer interrupted unpaired continuation retains its distinct
live/resumed/protected admission proof; it does not spend paired resume credit.

Keep browser lifecycle fixtures at the smallest valid corpus that exercises their
contract, with separate inventory coverage for full archives. The
[sticker removal fixture](../frontend/e2e/stickers.spec.ts) imports one unchanged
animated asset through the real ZIP path, verifies its sent bytes before and
after removal, and checks persisted removal after one cold navigation. Its
installation and panel scenarios retain the complete pack. Avoid a redundant
reload immediately before a helper that already navigates; preserve the scenario
deadline and the persistence assertion.

Browser device options express intent; they do not observe native input state.
Establish the required device mode through the browser's supported emulation
owner, then verify actual media queries and touch capabilities before measuring
responsive hit areas. Do not replace `matchMedia` with a JavaScript mock. The
[touch fixture](../frontend/e2e/touch-targets.spec.ts) exercises controlled native
state loss, actual CSS target sizes and a real tap through that same owner.
A negative control must observe the capability it removes; disabling touch does
not require every host to become hover-capable or shrink its CSS targets.

Before holding unrelated work in a full-host regression, observe the listener's
actual accept admission. Spawning a child is not proof it has been polled; work
in a scheduler-local slot may remain unavailable to other workers. Keep startup
readiness separate from the measured held-work progress condition.

A deadlock regression must not hang its own runner: bound coordination, arrange
fixture release even when assertions fail, and use an isolated process with an
outer deadline when a synchronous lock cycle cannot be safely unwound. An async
timeout inside the blocked thread is not sufficient. Avoid port-selection races
and process-global fixtures that assume serial execution; respect the repository's
test environment and resource controls.
For an ephemeral UDP endpoint, let the real socket owner bind port zero and
report the selected port. A released reservation is not ownership; reusable
discovery sockets can silently share the selected port with another runtime.
Participant preparation owns the actual IPv4/IPv6 port, and
`runtime_zero_discovery_port_owns_a_reachable_assigned_endpoint` in
[routed runtime tests](../crates/mesh-talk-core/src/node/runtime_routed_tests.rs)
requires distinct assigned endpoints and a real signed discovery response.

Intentional cancellation and output-limit controls need an independent process
owner. Inheriting the real runner's owner marker can terminate the enclosing
validation instead of only the synthetic command. The
[process-owner fixture](../scripts/diagnostics/windows-process-owner.test.mjs)
checks both the retained output-limit failure and successful enclosing supervisor
through an actual nested execution. When an assertion parses a child test report,
select its reporter explicitly; terminal detection must not change the machine
contract between TAP and human-readable output.

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

Saved-login verification controls must select a complete credential policy, including
failed-login cleanup and migration compensation. A mock that skips biometric reads
does not prove the OS will not ask to authorize a changed application binary. The
[desktop credential owner](../src-tauri/src/session_store.rs) and
[startup policy](../src-tauri/src/settings/startup.rs) keep convenience recovery away
from Keychain on success and failure. Their controls cover stale local passwords,
retired writes and locking during provider replacement; native acceptance additionally
requires two packaged builds with different code identities. Verify actual prompts,
not merely the absence of an explicit authentication call.

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
