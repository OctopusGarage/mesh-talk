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
application. This includes the explicit account File result/retry rigs: use the
existing independent peer owner for source, requester, recipient and the launched
fresh sibling, keeping inactive retry phases and every byte/manifest assertion.
Release fault helpers before joining hosts, and preserve body failure after
actual retirement. A shared-reactor timeout does not by itself prove starvation
caused that historical failure. The [own-copy control](../crates/mesh-talk-core/src/node/delivery_runtime_tests.rs)
holds both sibling workers after its durable copy completes and requires target
confirmation while they remain held; unrelated devices cannot share that stall.

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

**Evidence:** stall admission and a later network phase separately, use multiple
candidate routes, and verify permits/socket owners are released. Check that an
expired preflight has no partial durable mutation. Examples live in
[privacy runtime tests](../crates/mesh-talk-core/src/node/privacy_runtime_tests.rs)
and [group invitation tests](../crates/mesh-talk-core/src/node/account_group/tests.rs).

## R05: Bounded work must also make progress

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
Compare repeated exact queries with their first preparation too. A metadata-only
probe cache does not eliminate selection reconstruction in a different request
path. Reuse an exclusively owned, bounded prepared view only under its complete
current qualification; recompute mutable answers and preserve durability and
authority checks. The connection-owned custody regression in the
[participant network adapter](../crates/mesh-talk-core/src/node/privacy_sync/network.rs)
checks unchanged present/missing queries, unrelated and same-conversation appends,
authority revocation and selection release on disconnect. The ordinary Request
regressions in the [paged session tests](../crates/mesh-talk-core/src/node/session/paged_tests.rs)
require unchanged warm requests to perform no SQL mutations or commits, while
source appends, changed remote Have and revoked authority remain fresh. Reuse
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

Filter lightweight metadata before cloning large payloads. An empty final result
does not prove that a query avoided full-record reads. Durable selection indexes
must migrate and update in the same transaction as their authoritative records;
failed commits cannot acknowledge work. The customization storage regression in
[packs tests](../frontend/src/store/packs.test.ts) and real
[IndexedDB browser scenario](../frontend/e2e/customization-pending-storage.spec.ts)
checks empty pending reads, old-data migration and failed writes.

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

Batch limits do not bound repeated expensive validation within each row. Trace
the complete consumer path, including helpers that repeat another public query.
Request-local reuse may retain intrinsic certificate verification under a bounded
complete cryptographic key; it must not retain mutable membership, holder bindings,
document availability or final counts. Verify actual crypto invocation counts and
mutate authority or stored observations between rows. The custody batch regressions
in [node tests](../crates/mesh-talk-core/src/node/node_tests.rs) cover this boundary.

Reuse compiled static SQL through the existing bounded connection cache rather
than caching authorization results. Every execution must still bind the current
parameters and perform the original source, account and lifetime checks. Warming
a statement must not freeze a read snapshot, retain old bindings or suppress an
absence or storage error. Exercise changed parameters and retired owners after
warming the actual production queries. Intrinsic document memos similarly exclude
receipt-derived publication authority even when the signed payload is unchanged.

Suppress unchanged producers before entering those verified reads. A publication
gate must cover every effective input owner, retain the selected pre-work stamp
through bounded pages and leave newer changes dirty. Sparse integrity repair and
owned failure deadlines remain necessary; a failed pass is not completion.
The [local publication regressions](../crates/mesh-talk-core/src/node/account_sync/local_maintenance.rs)
count actual body reads and cover profile/proof/control changes, in-flight
invalidation, failed writes and timer-driven recovery without periodic peer polls.

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

A progressing serial test partition can exhaust its job watchdog without a hung
case. The Windows archive owner uses eight native serial execution partitions,
with direct required checks and the existing sealed once-only inventory contract.
Exercise actual pinned archive consumers and omission/duplication/failed-execution
controls. Preserve watchdogs, product assertions and full coverage. The existing
protection migration owner requires every successful current-head app-bound
replacement before retiring oversized partition names; skipped or partial runs
cannot qualify. Account for additional archive/setup cost and hosted variation.
The shared union verifier takes the caller-owned partition count; Windows capacity
changes must not alter the independent four-part ASan contract. Run both real-tool
protocol fixtures and their missing/duplicate/failed-receipt controls.

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

A periodic probe can miss readiness between its last check and the deadline.
Use [R05](#r05-bounded-work-must-also-make-progress)'s waiter protocol to recheck
the actual condition on a transition, retaining the original deadline and bounded
fallback. A snapshot taken after timeout is diagnostic evidence, not success.

The metadata-hold and repair-body-hold controls in the
[opaque mailbox regressions](../crates/mesh-talk-core/src/node/opaque_relay/incremental_receive_tests.rs)
qualify separate setup and admitted repair cancellation: setup cannot consume the
progress budget, and that budget must still cancel actual owned work.

A storage fault fixture must fail the currently selected persistence owner and
observe that failure before asserting recovery. A hook on a neighboring journal
cannot certify the actual commit failed merely because both use the same logger.

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
