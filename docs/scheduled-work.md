# Scheduled work and recovery

Use this guide when a feature needs work to run at a deadline, retry after a
failure, or reconcile after missing an event. The [event-driven design
guide](event-driven-design.md) owns event semantics and subscription rules.
The [performance guide](performance-requirements.md) owns resource budgets.

## Choose an existing owner

| Work | Existing entry point or reference | When to use it |
| --- | --- | --- |
| Refresh visible data from a scoped event | [useSerialPoll](../frontend/src/features/chat/useSerialPoll.ts) | A renderer view needs an initial read, event-driven refresh, a foreground catch-up and a visible fallback. The hook serializes actual reads, retains one pending invalidation and stops its timer while hidden. Select a supported invalidation scope and provide the feature's snapshot read. |
| Recheck an external setting on activation | [startRecoveryRefresh](../frontend/src/lib/recoveryRefresh.ts) | A renderer owner needs a coalesced foreground check. Set the interval to null for activation-only recovery, as used for native notification permission. |
| Enforce a UI read deadline | [readWithDeadline](../frontend/src/lib/readDeadline.ts) | The WebView needs to stop waiting for a read. Its shared admission limit stays occupied until the real native operation settles. Mutations and native cancellation have separate owners. |
| Retire expiring UI state | [Chat expiry](../frontend/src/store/chatExpiry.ts) and [contact expiry](../frontend/src/store/rosterRefresh.ts) | Data already supplies an expiry timestamp. Update the existing feature state so its owner can rearm the nearest deadline. The Host remains authoritative for business expiry. |
| Observe Host work after a runtime is installed | [NodeRuntime host-task registration](../crates/mesh-talk-core/src/node/runtime.rs) | The task belongs to an authenticated runtime. Register it with the runtime so retirement closes admission and joins owned work. Use graceful registration for adapters that need asynchronous cleanup. |
| Rate-limit a pending Host observation | [Native reception alerts](../src-tauri/src/reception_host.rs) | An existing watch is reliable. Await the watch and arm one delay only if a pending observation arrives before its next eligible time. |
| Refresh from a native event with a sparse safety check | [Network cost observer](../src-tauri/src/network_cost/mod.rs) | Native inputs may be missed or unavailable. The Linux and Windows adapter serializes the initial query, change-triggered queries and five-minute repair. macOS uses its native callback path. |
| Schedule retries for delivery | [Core delivery retry owner](../crates/mesh-talk-core/src/node/delivery_retry.rs) and [file delivery scheduler](../crates/mesh-talk-core/src/node/delivery_runtime.rs) | Work already belongs to delivery. Keep its destination keys, retry eligibility, worker limits and durable progress with that owner. |
| Combine multiple deadlines with changes | [Core profile publication loop](../crates/mesh-talk-core/src/node/runtime.rs) | A runtime owner already has change subscriptions, pending operations and semantic deadlines. Await the nearest known deadline alongside those signals. |

Reuse the entry point that already owns the business work. Reference examples
show the scheduling pattern to follow when a different lifecycle or authority
requires its own owner.

## Component contracts for new registrations

These contracts define how a shared scheduling component should behave when
features with matching needs adopt it. The table above links to callable
interfaces in the current code. Each runtime keeps its own executor and task
ownership.

| Component contract | Register with | Wake and completion |
| --- | --- | --- |
| Keyed deadline | Owner, work key, next eligible time and operation | Replacing a pending key updates its due time. Cancellation removes pending work. Recheck authority and deadline before an admitted operation acts. Park when the owner has no deadlines. |
| Serial refresh with repair | Owner, scoped wake, snapshot read, publish callback and repair policy | Register before the initial read when changes can race startup. Run one real read at a time and retain one dirty indication. On foreground, subscription lag or a repair deadline, read current state once. |
| Keyed retry | Owner, operation key, retryable outcomes, backoff bounds and the earliest permissible time | Keep the original work pending and retry one admitted attempt. Success removes the retry. Terminal failure, expiry or retirement stops it. Allow a dependency event to accelerate retry only within the owning policy. |
| Active cadence | Owner, start/stop conditions and overdue policy | Tie ticks to the active session or interaction. Skip obsolete ticks after stalls or resume unless the protocol explicitly requires another behavior. |

A registration should declare:

- **Identity and lifetime.** Name the runtime, account, view or operation owner
  and the smallest useful key. Define whether registering the same key replaces
  its work or requests one follow-up. Return a disposal path or register the
  task directly with its lifecycle owner.
- **Time model.** Use monotonic time for delay, throttle and backoff. Derive a
  wait from authoritative wall-clock deadlines for persisted expiry or leases,
  then recompute against that authority after resume or clock adjustment.
- **Execution budget.** Bound the number of pending keys and active operations.
  Define capacity behavior, such as folding invalidations together or deferring
  work to an existing durable queue. Keep database and network work outside
  scheduler locks. Independently verify current authority before committing.
- **Recovery.** Define the source of truth and the event or failure the repair
  covers. Make boot, reconnect, foreground and overdue work explicit. After a
  long sleep, reconcile the current state once and rearm from the remaining
  work, unless the owning contract requires processing each missed deadline.
- **Cancellation.** Distinguish removing a timer from retiring admitted work.
  A caller timeout cannot free capacity that the native operation still owns.
  Cancel or join outstanding work through the runtime owner. Reject stale
  completions after account, runtime or view replacement.

For example, a settings panel that reads sync status should register with the
existing serial refresh hook and select the relevant invalidation. The feature
provides its scoped query and current-owner result handler. Device linking
subscribes to same-owner sync status and retains its five-second visible fallback
for time-derived availability changes. The hook owns event
delivery, one in-flight read, foreground refresh, hidden behavior and cleanup.
A delivery retry instead registers with the Core delivery owner so it retains
durable work identity and the existing policy for eligible destinations.

## Refactor and verify a scheduling path

1. Record the current cadence, trigger, work per tick, overlap, stop condition
   and recovery guarantee. Count task wake opportunities separately from OS
   wakeups. Tokio timers and WebView timers run on their existing executors.
2. Convert revision-checking idle timers first. Next, compare duplicate
   foreground repair and deadline paths for matching lifecycle semantics. Trace
   Host account-profile projection and Core account reconciliation before
   changing them. Their storage and reconnect recovery need explicit proof.
3. Test a quiet window and return to idle. Cover a change while work runs,
   replacement of a pending deadline, failure and later recovery, late
   completion after retirement, and the applicable sleep/resume behavior.
   Use simulated time for scheduling contracts and native measurements for
   wakeups, CPU and latency.
4. Move two consumers to a shared component only after their policies match.
   Keep first and second caller tests, and record the owning contract and
   measurements. Reuse timers within an owner before considering coordination
   across independent executors or processes.
