# Performance requirements

Read this before changing background work, synchronization, storage, caches or UI
state. Mesh Talk should cost little while waiting, load data when needed and stay
bounded as history grows. This guide owns cross-cutting performance requirements;
[module ownership](module-boundaries.md), [network budgets](network-design-reference.md#8-resource-budgets-compatibility-and-performance)
and [reliability rules](ai-coding-reliability.md) own their implementation contracts.

## Budgets and measurement

These are initial project targets, **not industry standards or claims that the
current application meets them**. Establish a production-build baseline on each
supported platform. Reference fixture: 4 logical CPUs, 8 GiB RAM, SSD, 100 contacts,
20 groups and 100,000 stored messages; no pending transfers, calls or animations.
Separate cold start, warm start, catch-up, visible idle and hidden idle. Measure a
quiet five-minute window after warmup; record build, runtime, profile and workload.
Warm startup restarts the process with OS caches retained; cold startup records
its uncached/first-launch conditions. Usable means an actionable list or
conversation, not a blank shell; report human authorization waits separately.
The empty-shell comparison keeps the same backend, runtime, process membership
and window settings, with user payloads absent.

| Resource                | Initial target for the reference fixture                                                                                                                                                            |
| ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Idle CPU                | Visible desktop: ≤1% of one logical CPU; hidden desktop: ≤0.25%; headless node: ≤0.5%, averaged over the window.                                                                                    |
| Retained memory         | Headless node: ≤64 MiB private commit. Desktop: ≤64 MiB workload delta above the same-build empty shell; establish a reviewed whole-family ceiling from that baseline.                              |
| Memory growth           | Repeated navigation/media cycles reach a bounded plateau. Tenfold stored history must not require tenfold startup reads or retained UI payloads.                                                    |
| Idle writes             | Zero business-data commits for unchanged inputs. Target ≤5 MiB/hour OS-observed attributable storage writes per application family after warmup; measure the full hour before claiming this target. |
| Background wakeups      | No application maintenance timer without pending work, a real deadline or a documented recovery purpose. Noncritical recovery defaults to ≥5 minutes and coalesces activation.                      |
| Startup and interaction | First usable shell ≤1 s warm / ≤3 s cold; ordinary interaction must not run blocking I/O, crypto or an unbounded traversal on the UI thread. Report hydration/catch-up separately.                  |

CPU means `100 × process CPU seconds / wall seconds`, summed over the full app
family; divide by logical CPU count only when reporting whole-machine percent.
Report Rust host **and** renderer/GPU/helpers. Distinguish private commit, private
resident memory, shared pages and live JS heap; on Linux use PSS for shared-page
attribution. Logical I/O includes IPC/network activity and is not physical disk
I/O; OS disk events also do not measure physical NAND write amplification. CPU alone does not establish battery/power efficiency: inspect wakeups, GPU,
network and actual energy where supported. Keep metrics collected in different
windows separate. Store measurements in PR/CI artifacts, not this guide.

### Desktop acceptance

Use the desktop CPU, retained-memory and startup figures above as optimization
targets. A desktop change can be accepted above those figures when affected
functional and recovery checks pass, there is no known blocking defect, and
comparable measurements show no material regression against the platform's
established baseline. Record the build, measurement conditions, observed gaps
and any missing platform evidence in the PR or CI evidence.

Treat reproducible freezes, data loss, broken recovery, unbounded growth and
repeated work on unchanged inputs as defects. Investigate a sustained resource
regression and attribute it before accepting a baseline change. Keep headless
and post-office budgets in force. Migrations, catch-up and active media have
their own bounded work budgets and progress evidence.

## Headless post-office nodes

Apply the headless CPU and private-memory targets to the dedicated post-office
process, including its attributable helpers. A relay stays available to receive
connections, serve mailbox requests and maintain required protocol leases while
quiescent. Its steady idle work must remain bounded as retained ciphertext,
offline participants and configured federation peers grow. An empty store alone
cannot establish the retained-workload result.

- At startup, restore only the indexes, authority and bounded cursors needed to
  accept traffic. Keep migrations, backlog catch-up and recovery measurable
  separately. Report time to accepting requests, peak memory and I/O.
- In steady idle, wait for ingress, committed mailbox or topology changes and
  the next real expiry, lease or retry deadline. Unchanged federation inventories,
  empty drains and repeated unreachable peers must settle or back off. Sample
  both an empty relay and a populated relay after warmup, including CPU, memory,
  wakeups and disk writes. Observe the full hour to claim the write target.
- Under traffic, measure delivery and custody latency, queue depth, memory,
  CPU and I/O. Verify bounded batches, overload handling, restart recovery,
  federation convergence and return to idle. Keep receipt meanings, retention,
  authenticated scope and convergence guarantees intact.

Use the existing [relay resource contracts](post-office-federation.md#indexed-relay-runtime-and-resource-bounds)
and [event and scheduling owners](event-driven-design.md#schedule-work-by-purpose).
Record platform, binary, topology, load, sampling window and missing evidence
alongside every result.

## Apply to every feature

Before implementation, identify the owning producer, its meaningful trigger,
bounded work unit, retained resources and stop condition. Review all three phases,
including costs introduced in another process or on another device:

| Phase           | Required design and evidence                                                                                                                                                                                                                                                                                                                                                                                             |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Startup         | Load indexed summaries and the active conversation's bounded page first. Defer inactive history, attachment bytes and optional features until demanded. Avoid decrypting all history, rebuilding unchanged indexes or republishing unchanged state at each login. Measure time to usable UI, peak memory, bytes read and writes separately from catch-up; migrations need resumable bounded progress.                    |
| Idle            | With unchanged inputs, settle business producers and avoid business-data writes. Use scoped events and exact next deadlines; coalesce noncritical repair into the existing serial queue, normally at least five minutes apart. A heartbeat, read, empty reply or offline device must not restart historical work. Verify visible and hidden states, including renderer/helpers and remote work triggered by this client. |
| Active business | Bound each query, decrypt, decode, transfer and rendering turn by items and bytes. Drain real available work promptly with backpressure and cancellation; user interaction and reception must remain responsive. Measure latency, peak CPU/memory and I/O for send/receive, catch-up, search, media and navigation as applicable, then prove resources settle when work finishes.                                        |

Do not classify a permanently retrying historical queue as an unlimited active
workload. Distinguish actionable progress from blocked work, record its outcome
and retain a bounded retry deadline. Successful reads, metadata comparisons and
already-held custody are not new user data. Serializing unnecessary work or
lengthening a timer alone does not establish that its cost is justified.

For every affected phase, the PR must state the trigger and stop condition,
resource bound, selected regression and measured result or explicit evidence gap.
Exercise unchanged inputs, growing history, unavailable dependencies and owner
replacement where applicable. Compare the same workload before and after; verify
return to idle and memory reclamation after repeated use. Do not infer low power
from a quiet disk, fast startup from a blank window, or bounded memory from one
snapshot. Existing misses remain explicit follow-ups with an owning module and
reproduction; never describe a passing correctness test as performance acceptance.

## Design and review rules

1. **Trigger work from meaning.** Prefer committed changes, scoped invalidations,
   user navigation and foreground activation. Schedule the next actual expiry or
   retry deadline. Empty success is not progress and must not keep a fast retry
   hot. Coalesce duplicate wakes; retain bounded dirty work while a task is busy.
   Before adding a subscription or timer, use the [event-driven design guide](event-driven-design.md)
   to locate the owning event and scheduling mechanisms. Reuse scoped watch
   signals for projections that can read current state. Use the existing lifecycle owner for deadlines,
   throttling, retries and periodic repair. Document the trigger, cancellation
   boundary, missed-signal recovery and the reason for any recurring idle work.
   Native reception alerts follow the core UI-generation watch and reserve a
   one-shot delay only when a pending change arrives before the next allowed
   observation. Durable reception remains independent.
   Pending work still owns its retry deadline. Suppress unchanged ordinary wakes
   using scheduling evidence, independently of successful completion; preserve
   explicit drivers and event-only progress when no timer owns continuation.
   A retained blocked task is not evidence of progress: keep its recovery fact
   while backing off, and resume promptly on dependency changes. Local pagination
   and actual received bytes retain prompt continuation.
   Reconfirming an already-held relay copy is not new progress. Park unchanged
   successful reconciliation until sparse repair; keep authenticated recipient
   delivery and actual custody-loss recovery separate from that scheduling hint.
   A bounded historical scan must also become quiet after its complete sweep;
   preserve prompt unfinished pages and separate lightweight claim rechecks.
   Qualify scan completion against current source and semantic inputs, before
   and after work. Failed discovery keeps a separate bounded wait; it cannot
   acknowledge EOF or repeatedly hydrate the same blocked item on observer wakes.
   Preserve invalidation domains through batching and acknowledgement; a roster
   heartbeat must not reload unrelated settings or history. Empty pages do not
   imply a global change. Presentation metadata does not renew user activity.
   Repeated reception from an unmapped device is a scheduling hint, not proof
   that a new contact became eligible. Bound same-device rechecks with one
   trailing deadline while keeping first distinct devices prompt. A continuous
   same-source burst must not invalidate every in-flight read; publish an
   admitted snapshot and retain its bounded follow-up. The contact controls in
   `frontend/src/store/chat.test.ts` and `rosterRefresh.test.ts` cover settled
   bursts, sustained input and retirement.
   Keep semantic scope through auxiliary listeners too: document and sync-status
   changes must not query every group, and status-only changes must not reload
   document inventories. Mixed changes retain every affected domain; unknown
   inputs use conservative repair rather than silently dropping work.
   Retry fairness is scheduling state, not durable progress. Use a bounded
   runtime order where possible; retain durable pending/completion evidence and
   an explicit capacity fallback. Rechecking blocked work must not itself renew
   durable tickets indefinitely.
   Compare canonical endpoint sets rather than preferred-route order. Renewing
   an already-known route must not restart synchronization; actual topology,
   role, account and privacy changes retain their invalidations.
   Apply that projection consistently across retry, continuation and publication
   consumers; otherwise an unchanged observation can still reset another lane.
   Advance semantic revisions only when their authoritative value changes.
   Emit presentation invalidations only for changed facts; successful idempotent
   confirmation must not restart UI reads or persistence. Capture change evidence
   inside the committing owner and notify after releasing its guard.
   Subscribe to high-frequency observations in their small status surface rather
   than the message viewport's parent. A changed peer-check timestamp must update
   its own label without rebuilding unchanged virtualized rows; message, expiry
   and conversation/session changes retain their independent subscriptions.
   Notification context subscribes only to the active conversation and canonical
   matching device set. Peer freshness, names and unrelated roster changes must
   not clear and republish the same native context or rerender the chat shell.
   Replacing an identical binding map is not invalidation; actual removal and
   later restoration must each invalidate their preceding observation.
   Subscribe before the initial read, retain one follow-up for changes during
   work, and gate settled credential reconciliation before secret decoding and
   applied-marker I/O. Pending, locked and failed work retain recovery; event
   hints never replace fresh authorization at application and commit.
   Reuse identical intrinsic signature facts in a bounded metadata owner, keyed
   by every verification input and signature. Cache successful facts only; keep
   live identity, policy, revocation and freshness checks at their owning boundary.
   Contended or unavailable caches must not introduce a blocking dependency.

2. **Use recovery deliberately.** Ordinary fallback maintenance should drain a
   bounded, coalescing queue serially through its existing lifecycle owner.
   Prefer one admitted worker; justify additional parallelism with a concrete
   latency or progress requirement. Queue only relevant dirty work or due repair,
   deduplicate by scope and skip missed timer bursts. Bound each turn and rotate
   unfinished work fairly; an unavailable peer must not monopolize the queue.
   Keep synchronous I/O/crypto off the UI/async event thread, and retain capacity
   until the actual worker finishes even if its observer is cancelled. Preserve
   independent deadlines for transport leases, active delivery and real expiry;
   trace dependencies before sharing a worker to avoid cross-lane wait cycles.
   Serial execution bounds peaks; unchanged inputs must still avoid the work.
   Retire superseded producers when consolidating maintenance. Check the actual
   runtime wiring: an unchanged legacy timer can keep duplicating an otherwise
   event-driven replacement. Durable historical facts do not themselves mean
   that another retry is due; scheduling owns its separate completion/deadline.
   A fallback repairs missed events; it is not the
   main data path. Back off failures, stop with the owner and refresh on return.
   A stored address or linked-device certificate is not current availability.
   A stale dependency is not preparation progress: retain its backoff instead
   of repeatedly making the item immediately runnable. Wake only matching
   waiters when the owning dependency publishes readiness; hints grant no access.
   Apply an existing retry hold before resolving routes or reading consent for
   work which cannot be admitted yet. Keep expiry and meaningful release at the
   scheduling owner; due admission and sends still require fresh authority.
   Completion of one subgoal need not complete a durable intent. After exact
   relay custody, retain recipient demand but park repeated reconciliation behind
   relevant availability changes and sparse repair. Capture event baselines
   before work and compare at completion; a change during work must not be
   absorbed into a fresh quiet baseline. Heartbeats cannot renew the deadline.
   Failed preparation must not monopolize the oldest unfinished scope. Retain a
   bounded error hold at the existing work owner, include its deadline in the
   scheduler, and let healthy scopes advance. Serialize hold installation with
   matching dependency wakes so a concurrent recovery cannot be lost. Repeated
   observations cannot renew the hold or turn it back into immediate work.
   Park offline peer work without discarding receiver progress; resume on fresh
   eligible evidence. Send bounded change hints to available siblings and let
   each receiver own its incremental pull. Local maintenance notifications and
   unchanged presence heartbeats must not restart every sibling's history scan.
   Separate expensive provider inventory reads from lightweight protocol lease
   probes; cached endpoint hints grant neither presence nor admission. Events
   refresh the inventory promptly; providers without topology events need an
   explicit sparse-repair discovery limit.
   Keep one next-wake decision in the existing owner, derived from eligible work,
   actual capacity, expiry and fixed repair. A due retry behind held capacity must
   wait for completion rather than spin on its past deadline.
   Retained terminal records do not justify fast observation. Use actionable
   demand and unfinished cursors, retain failed or capacity-deferred work and
   changes arriving during reads, then fall back to sparse discovery when quiet.
   Native delivery, discovery leases, calls and real expiry retain their own
   timing contracts; never slow them merely to meet an idle chart.
   Reuse independent fresh discovery before optional authentication probes, with
   exact identity/endpoint qualification and a deadline that reserves the whole
   handshake budget. A skipped probe cannot extend presence or grant access.
   Gate quiet recovery before entering its business queries. Replace a blanket
   tick only after the owner exposes bounded continuation, failure retry and
   dependency-blocked outcomes; unfinished pages must still drain promptly.
3. **Read metadata before payloads.** Compare revision/digest/cursor and query
   pending keys before reading, decrypting, cloning or decoding bodies.
   Reject impossible attachment candidates before canonical history projection;
   selected candidates still require fresh protection and final eligibility.
   Paginate changes; drain available pages promptly in bounded batches. A slower fallback
   must not delay each already-available page by another interval.
   A memo that still hydrates its input on every wake is not an I/O optimization.
   Check idempotent updates against borrowed current records before cloning a
   directory or serializing a replacement. Within one bounded snapshot, index
   complete device/account bindings once instead of repeating pairwise searches.
   Preserve fresh policy, exact-key checks and save-before-publication failures.
   Compute bounded metadata content digests at their mutation owner, lazily after
   invalidation, rather than cloning and sorting unchanged inputs on every query.
   Retain only the digest; include complete canonical content and owner identity.
   Keep lease/route notifications, current permission and source checks independent.
   Reject exact peer-known IDs before per-item export lookup or body hydration;
   still validate the current view/owner and charge examined rows/cursor progress.
   Use the existing bounded SQLite statement cache for fixed hot queries. Rebind
   parameters and read current rows each time; compiled statements are not cached
   authority values or permission decisions.
   Within one synchronous connection-owned validation, read shared scope and
   retirement state once and combine compatible indexed scalar observations.
   Repeat validation at the next operation and final live-authority boundary;
   transaction-local reuse must not become a cross-request permission cache.
   Place the cheap revision gate before routing filters, list hydration and
   authority projections. Mark a cycle settled only when every selected item
   has matching successful evidence; preserve continuation and retry deadlines.
   Reuse authenticated immutable input within one decision instead of reading
   and verifying it again down the call chain; keep current authority and read
   failure checks independent of that reuse.
   Prepare shared lookup context once per bounded decision, lazily when needed;
   do not rescan account history for every missing attachment. Keep per-item
   reads and recall/eligibility checks current and build fresh context at final
   publication. A scheduling snapshot is not commit authority or a durable cache.
   Keep this reuse inside the decision when metadata alone cannot establish
   that durable bytes still exist and pass integrity checks.
   Local selected/absent-target preparation may survive relay replacement under
   current source and policy fences; remote custody still needs its fresh probe.
   Reconnection is a delivery trigger, not a new content revision. Reuse a
   retained immutable publication only with exact, bounded sender provenance
   committed alongside that event. Do not infer plaintext equality from
   randomized ciphertext or timestamps; missing evidence uses normal recovery.
   Gate unchanged producers using complete input revisions; acknowledge the
   selected revisions, retain failed work and schedule sparse integrity repair.
   Separate volatile evidence from stable projections: refresh presence without
   rehydrating group history or rereading account rosters. Qualify reuse only after
   successful current publication within the runtime owner; failures remain retryable.
   Validate a complete reply before caching its token.
   Coalesce independent consumers of the same event at the actual read owner.
   Keep the observation lazy and scoped to that event and runtime, with only the
   latest pending invalidation retained. Separate expensive optional projections
   so they cannot block the shared cheap result. Preserve original evidence age,
   independent publication order and explicit errors; a cross-event TTL cache
   cannot replace fresh authority or presence.
   Distinguish failed work from a qualified unchanged round. Empty output alone
   proves neither. Recheck exact source and permission evidence after awaited
   stages; incomplete, denied or unknown work retains recovery. A quiet scheduling
   result is not delivery, durable custody or permanent completion.
   Unequal inventories are not necessarily unfinished work. A receiver-only
   comparison may reuse a completed zero-transfer result only with proof that
   every remote advertised ID is already local, exact current local authority,
   a fresh remote digest match and a bounded nonrenewing deadline. Preserve the
   remote intersection count; never infer equality, custody or delivery. Unknown
   or changed inputs use normal reconciliation. Keep the hint in the existing
   metadata quota and account for its hashing within owned inventory budgets.
   Apply already-authoritative metadata exclusions before body reads, including
   inventory rows that a physical local group would later discard.

4. **Bound every retained layer.** Budget bytes/pixels as well as entry count,
   concurrency and queues. Virtualized DOM is not a bounded data store. Keep a
   small active window; evict inactive payloads and load originals only on demand.
   Closed feature imports must not eagerly load their data through another caller;
   defer the producer as well as its component, with scoped cancellation and retry.
   Library catalogs and background registration receipts must not retain image
   URLs or archives. Scope point reads to content revisions, bound concurrent
   hydration and release inactive payloads; readiness belongs to the selected asset.
   A bounded native cache does not justify eagerly filling it from an entire
   library. Query existing pending metadata first, hydrate only matching assets
   and permit re-demand after eviction. Keep continuation through filtered pages;
   a row limit alone does not bound nested per-row history traversal.
   Bound negative caches as well as successful results; eviction permits a later
   retry instead of retaining every failed identifier until logout.
   For fully version-qualified derived plans, elapsed idle time alone must not
   force durable cleanup/rebuild. Bound retained resources by entries and bytes;
   invalidate on authoritative change, capacity pressure or owner retirement.
   Time-based credentials, delivery deadlines and network leases remain subject
   to their own current expiry checks.
   Compressed file size is not decoded image memory. Avoid keeping archives,
   base64 strings, Blobs and decoded originals together without a measured need.
   Reuse expensive unchanged presentation work with bounded semantic keys. Keep
   renderer collection and row references when a refresh yields the same facts,
   so store subscribers can skip rendering. Delivery projections still clear
   obsolete observations and preserve terminal receipts.
   Locale and fresh local calendar fields must still qualify date/time results; a cached
   formatter's original timezone must not pin later output. Cache display strings
   without retaining message identity or adding another timer.
5. **Write only durable changes.** No-op reads/heartbeats must not rewrite logs,
   snapshots or indexes. Checking a blocked retry must not persist another scheduling ticket.
   Keep immutable admission/completion facts and bound runtime retry cursors;
   scoped cursor eviction must preserve global recovery fairness. Changes to
   disposable query work, channel queue scheduling or optional transport checkpoints must not invalidate otherwise unchanged semantic work
   hints. Enforce that exclusion at the SQL table/transaction owner, preserving
   current authority, default write fences and rollback behavior.
   Checkpoint replayable cleanup scans once per bounded page, retaining fresh
   reference/revision checks and durability for each actual deletion. A failed
   page may repeat its bounded reads without losing accepted data or skipping
   unresolved entries. Do not fsync a progress cursor for every protected file.
   Batch safe related mutations under the existing durable
   transaction; atomically update derived indexes. Bound WAL/log maintenance and
   preserve crash recovery and acknowledgment semantics when reducing flushes.
   Treat sync preparation as part of query cost: a fingerprint mismatch is not
   evidence that the local source changed. Reuse the existing exclusively owned
   authorization selection across query phases while its complete source and
   policy qualification matches; recheck current authority before exporting.
   Own reusable derived indexes at the node/version lifetime; verify unchanged
   costs across fresh adapters and fresh sockets. Separate active-query admission
   from idle index quotas, discard finished reducers/payloads and retire extracted
   indexes outside cache locks. Close publication before joining actual cleanup.
   A successfully answered query is not received data. Dispatch reception from
   admitted payload changes; metadata, Have/custody queries and empty frames must
   not advance retry queues, refresh messages or schedule profile recovery.
   Reuse free database pages before compacting small temporary allocations;
   physical reclamation needs its own size/ratio trigger and low-duty budget.
   Zero changed SQL rows does not exclude vacuum or checkpoint writes.
   Discovery watermarks advance only over new input. Keep cyclic rediscovery
   positions volatile; elapsed checkpoint time cannot turn an old scan into new
   durable progress. Signed consent decisions retain their separate durability.
   Retry selection must not renumber an entire queue that already fits one turn;
   preserve fair rotation under contention and exact completion revisions.
6. **Plan for accumulation and deletion.** Give history, attachments, indexes,
   retries, diagnostics and deletion evidence an owner, quota, reclamation trigger
   and recovery rule; warn before configured capacity is exhausted. Reuse freed
   storage and batch compaction instead of rewriting the database after each delete.
   Evicting a cache must not resurrect deleted messages or lose
   unread/receipt evidence. Compact tombstones only when a durable floor/epoch or
   equivalent protocol proof prevents old replicas from reintroducing data.
7. **Make inactivity and retirement explicit.** Distinguish visible, hidden, call
   and stopped states. Release unused media and subscriptions. Prove reception,
   persistence and notifications survive before suspending a page. Actual work
   retains its permit until completion; stale owners cannot publish late results.
   On Windows, page visibility can remain visible after native minimization.
   Visible-only renderer polling observes the native window state and resumes
   on restoration. Durable reception retains its own lifecycle.
   Keep indefinite idle states such as discovery without peers visually static.
   Verify that they do not schedule continuous animation frames or compositor
   work. Use the [presence browser scenario](../frontend/e2e/person-presence.spec.ts)
   and native renderer sampling when the state is shown by a desktop WebView.

## Desktop resource monitor

Open Diagnostics from the tray or sidebar and enable sampling in the Overview
tab. Sampling uses the visible serial refresh owner at three-second intervals.
Closing or hiding the panel, minimizing the window or turning sampling off
releases its lease. An abandoned lease expires after five seconds without a
refresh. The feature adds no collector, persistence or probe traffic. Sample
baselines and traffic totals reset with each observation. The Overview also
shows the desktop host readiness duration and, when the renderer acknowledges
initial presentation, the elapsed time to that acknowledgement from the same
launch clock. The acknowledgement measures preparation of the initial surface,
which can be the sign-in screen. The ten-second visibility fallback produces
no acknowledgement. Measure the first actionable conversation separately in
startup acceptance. These startup milestones are available without sampling.

- **CPU and memory** cover the desktop host and up to 31 descendants identified
  by platform process data on macOS, Linux and Windows. CPU shows the change
  in summed CPU time relative to one logical core. It needs two readings of
  the same process identities within six seconds. Resident memory sums process
  residency and can count shared pages more than once. OS-owned WebKit helpers
  outside the process tree are outside this attribution. The panel shows each
  observed process and retains one scalar peak for residency across complete
  samples in the current observation.
- **Writes** are cumulative for the currently observed processes. macOS and
  Linux expose attributed disk-write bytes. Windows exposes logical process I/O
  writes, which include other I/O. Missing counters display as unavailable.
- **Connections** count live authenticated channel objects owned by the app.
  Multiple channels can belong to a single peer. **Traffic** counts successful
  encrypted application frames and their length prefixes during the sampling
  lease. It excludes handshake bytes, discovery packets, media and TCP overhead.
- **Rates** use two existing samples and their monotonic native timestamps,
  captured after the sampler enters the blocking pool. Process write
  rates require matching process identities and monotonic counters. Transport
  rates require monotonic byte counters from the current observation. A
  missing baseline, counter reset or interrupted interval leaves the rate
  unavailable. The panel retains no time series.
- **Peer latency** uses the last successful signed routed-discovery
  request/reply measurement within its presence lifetime. The Peers tab
  displays the sample age, or indicates that no recent RTT is available.

The expandable **Message activity** view uses the same lease. Folded details
stay unmounted. The native snapshot reads direct backlog only while message
details are open. File receive observations reuse the manifest validated by the
commit owner. Local acceptance is
a newly committed outgoing intent. Outbox acceptance failures include validation,
capacity and journal errors. A failed desktop command is also shown separately
and can overlap an outbox failure. First authenticated direct receipts count
once per logical message.
Attempt timeouts and scheduled retries are exact-owner attempt outcomes; they
do not turn pending messages into failed messages. Route restarts count exact
active destinations cancelled on a changed route set. Direct storage and
post-office custody count observed storage outcomes, independently of recipient
receipts. The direct backlog covers the installed outbox. A busy writer or more
than 1,024 entries can make the
oldest age unavailable. Receipt latency uses the local message timestamp and
fixed buckets from 50 ms through 10 seconds, with an overflow bucket. Clock
rollback omits a latency sample.

Newly committed private and group receipts show counts and bytes by text,
sticker, media and attachment. Text uses body bytes, stickers use identifier
bytes, and attachments use manifest-declared size. Catch-up is a subset.
Recognized history and own-device arrivals roll into their own counters.
Previously processed direct events are counted at the delivery deduplication
boundary. Explicit attachment decisions count changes across equivalent signed
offers.
Verified incoming attachments use completed transfer evidence and verified
payload size; multiple cards for the same retained transfer do not multiply
it. The displayed verified throughput is bytes newly completed in the sample
window. It can be bursty for large transfers. No message content, account
identifiers or per-message sample history are retained. Group recipient delivery, route-specific delivery attribution and
network retransmissions have separate meanings and must not be inferred from
these aggregates.

## Core business metric coverage

Use the business owner below to choose the capture point. This inventory records
coverage and gaps. A proposed metric needs an executable test at its owning
boundary before it becomes a supported dashboard measure. The
[event contract](event-driven-design.md#choose-the-right-kind-of-signal)
distinguishes committed facts from coalesced wakes and observations.

| Business area | Existing diagnostic evidence | Metrics to add and event owner |
| --- | --- | --- |
| Startup and profile lifecycle | Desktop host ready and initial interface preparation | Login outcome and time, authenticated runtime install, first actionable conversation, hydration duration, lock, logout and owner retirement. Use authentication results and the active [runtime installation owner](../src-tauri/src/chat_commands/state.rs), then the renderer's first usable state. Time user authorization independently. |
| Routing, discovery and transport | Authentication channel count, encrypted application bytes, discovery RTT and route-related delivery cancellations | Successful and failed authenticated connects, reconnect duration, eligible route changes, presence lease expiry and time to recover. Record attempts at [session admission](../crates/mesh-talk-core/src/node/session_pool.rs) and route transitions at the authoritative [roster](../crates/mesh-talk-core/src/discovery/roster.rs). |
| Direct and group messaging | Local acceptance, direct receipts, failures at acceptance, retry timeouts, direct backlog, fixed receipt latency buckets and received volume by type | Capture final expiration or cancellation where the owning delivery record retires. Time a nonempty backlog episode from recovery start to empty. Group recipient delivery requires qualified recipient evidence. Route-specific delivery latency requires the path that produced the accepted receipt. See [delivery](../crates/mesh-talk-core/src/node/delivery_runtime.rs). |
| Attachments and inline media | Declared offer bytes, approval or decline, verified completions and interval volume | Transfer starts and terminal outcomes, retry or cancellation, active transfers, transfer age and time to verified completion. Use the transfer worker and [durable completion owner](../crates/mesh-talk-core/src/node/file_completion.rs). Keep network bytes, verified bytes and local save errors distinct. |
| Calls | Transport activity; call transport itself is outside encrypted application frame totals | Offer, ring, answer, connection, timeout, user cancellation and disconnect outcomes from the [call state owner](../frontend/src/store/calls.ts). If needed, sample WebRTC quality while connected using the active call's existing ownership. Retain setup-time and active-call quality separately. |
| Account and linked-device sync | Observed history, catch-up and own-device arrivals in the monitor | Accepted, unchanged, rejected or deferred pages and rows/bytes applied, catch-up/repair age, conflicts and retry counts. Capture from [account sync's apply boundary](../crates/mesh-talk-core/src/node/account_sync.rs). Record device linkage and key changes at their authenticated [linking owner](../crates/mesh-talk-core/src/node/linking.rs). |
| Local history, search and storage | Process write samples | Search latency and outcome, empty/cancelled requests, bounded result counts, history page hydration, committed writes and retention/expiry rows and bytes. Capture search completion in [owner search](../src-tauri/src/owner_commands/search.rs) and cleanup outcome in [expiry](../crates/mesh-talk-core/src/node/expiry_policy.rs). |
| Recovery and migration | Existing recovery validation and startup milestones | Backup/export/import and migration start, completion or failure, elapsed time, verified rows and bytes, resumptions and retired work. Reuse [recovery](../crates/mesh-talk-core/src/node/recovery.rs) results and validated progress checkpoints. |
| Post-office mailboxes | Existing bounded diagnostic [mailbox work observations](../crates/mesh-talk-core/src/node/work_observation.rs) | Accepted, duplicate, rejected, expired and reclaimed envelopes, bytes held, queue depth and oldest eligible age, pull outcome and time to drain. Use the durable custody and cleanup owners in [post office](../crates/mesh-talk-core/src/postoffice/mod.rs). Relay counters describe opaque envelopes. |
| Post-office federation | Relay work observations and existing reconciliation revisions | Session outcomes, committed pages and bytes, unchanged rounds, bounded retry age, expiry and convergence time. Use the [federation sync outcome](../crates/mesh-talk-core/src/postoffice/federation.rs) and existing history revision. Separate peer authorization failure from healthy idle. |
| Integrations and updates | Existing operation state, audit and updater progress | Integration requests by fixed operation, allow/deny, rate limiting and result; update checks, download/verification/install outcome and duration. Reuse [integration dispatch](../crates/mesh-talk-integrations/src/service.rs) and [desktop update outcomes](../src-tauri/src/updates.rs). |

### Capture and performance contract

- Record work only at an owner that knows its outcome. Existing revision or
  notification streams can wake a panel to read a summary, but their delivery
  and coalescing behavior cannot establish counts or business success.
- Reuse owner counters and monotonic elapsed times from completed operations.
  For current state, use an existing bounded snapshot. Avoid copying payloads,
  reopening databases, rescanning retained events or queuing one notification
  per operation merely to produce statistics.
- Keep fixed dimensions per process role and observation session. Useful
  dimensions include operation class, direct/group, outcome, and trusted route
  class when evidence provides it. Exclude contents, raw IDs and addresses.
  Long-lived results may retain a bounded histogram and sample count. Separate
  cancellations, retries, missing measurements and explicit failures.
- The existing desktop observation lock serializes frame counters and business
  totals while sampling is enabled. Treat it as a performance constraint before
  adding high-frequency owners. Benchmark same-build off/on comparisons before
  expanding the hot path. Prefer owner-local aggregation or batched publication
  when contention is measurable. A late update from a retired observation must
  not be applied to a new observation.
- Keep relay measurements on the relay and desktop measurements on the
  desktop. Reuse the existing visible serial refresh for desktop presentation
  and existing relay diagnostic output or an explicit snapshot for headless
  use. Do not start a background collector just to serve a dashboard.
- Validate idle, burst, failure, duplicate/replay, scope retirement and
  recovery. Measure disabled overhead and observer-on cost separately,
  including CPU, wakeups, memory and business tail latency. Count success
  against a correctness witness such as the authenticated receipt or verified
  recovered bytes. A synthetic microbenchmark measures a code path, not the
  cost of an entire installed app or relay.

## Evidence and maintenance

Before optimizing, identify the owner and capture the offending operation count
or allocation/CPU/write trace. Add a regression that fails on that mechanism,
then validate empty/no-change, growth, failure/retry, cancellation and recovery.
Isolate the native profile and the renderer's persistent storage together for
each desktop fixture. On Windows, select a task-owned WebView2 user-data folder
explicitly. Reusing browser storage with a copied or rolled-back native profile
can carry a saved reception cursor from a different run. Check that the
fixture's durable reception is healthy before classifying its work as idle.
For Linux desktop measurements in a VM or container, place the writable
profile and WebKit cache on a guest-native filesystem with free space.
Keep host-shared mounts for read-only fixture inputs and exported evidence.
Record the filesystem and cache conditions when comparing resource results.

Compare the same device/build/runtime/profile; include whole-family resource
measurements and a correctness witness. SQL affected-row counts do not measure
write amplification: attribute pager flushes separately when they cannot explain
OS-observed WAL activity. Keep such diagnostics opt-in, bounded and free of SQL
text, parameters and message content. IPC diagnostics count registered dispatch
attempts by bounded command name, without arguments or results; they do not prove
command completion or CPU attribution. Keep their collector and output disabled
by default, and emit after releasing observation guards without adding a timer.
Record startup timings at their actual milestone. The desktop log records
`desktop_host_ready_ms` once when Tauri signals readiness after setup. Measure
first usable UI, authentication and account hydration separately.
For routine Linux feedback, use the [headless idle benchmark](evals/README.md#linux-headless-idle-resource-benchmark).
Its short window and fresh profile are diagnostic. Compare the same runner and
build profile before tightening budgets or using results for release acceptance.

For macOS desktop attribution, confirm process membership from the launch
cohort and app-scoped WebKit evidence. Record host and helper counters over the
same elapsed interval. Use `sample` to identify active Rust and WebKit stacks
and `vmmap -summary` to partition renderer memory into graphics, allocators and
other mappings. Waiting threads dominate wall-stack counts, so those counts
cannot apportion CPU time. Keep resident, compressed or swapped, and reserved
bytes separate. Collect an isolated same-build empty-shell comparison and
retaining paths before assigning renderer memory to specific application state.
Inspect automation components for work that remains active between commands.
The macOS WebDriver fixture installs a repeating main-run-loop pump for
off-screen automation. Use its resource counters for matched fixture diagnostics
and use the shipping entry point for the production idle budget.

Heap snapshots/retaining paths distinguish
leaks from stable bloat and allocation churn. Forced working-set trims, periodic
GC, cache deletion or moving pressure to disk do not prove a fix.

When a new defect exposes a reusable pattern, extend the relevant rule here and
its owning capability/test selector. Keep each addition to mechanism, requirement
and regression reference; do not append incident histories, benchmark diaries or
duplicate feature contracts. Follow the [validation protocol](testing-feedback-loop.md).

Primary references: [WebView2 performance](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/performance),
[Repeated locale formatting](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Date/toLocaleString),
[Edge memory diagnosis](https://learn.microsoft.com/en-us/microsoft-edge/devtools/memory-problems/),
[IndexedDB clone semantics](https://developer.mozilla.org/en-US/docs/Web/API/IDBObjectStore/getAll),
[SQLite WAL](https://www.sqlite.org/wal.html),
[SQLite reclamation](https://www.sqlite.org/lang_vacuum.html),
[Windows power efficiency](https://learn.microsoft.com/en-ca/windows/apps/performance/power),
[Apple timer guidance](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html),
[Matrix incremental sync](https://spec.matrix.org/v1.19/client-server-api/#syncing),
[TDLib cache reclamation](https://core.telegram.org/tdlib/docs/classtd_1_1td__api_1_1optimize_storage.html).

### Compatibility group recovery budget

Production `node::session::request_sync_turn_paged` negotiates bounded range turns.
Only unsupported old peers use private `request_legacy_turn_paged`, capped at 200
transferred events, 1 MiB encoded bodies and eight steps. Preparation yields after
four bounded steps; mismatched Have inventory is limited to 2,000 IDs/eight frames.
Cached equal fingerprints can finish without exchanging Have. Oversized responses
are rejected before apply; interrupted legacy channels retire without completion
evidence. The public `request_round_paged` is an explicit legacy drain API and is
not the production default. No remote latest-200 or seven-day history window is
implemented: the frontend's existing 200-record page is a separate presentation
limit. Cold authorized preparation and index construction remain O(N) overall,
spread across bounded turns; subsequent qualified ordinary appends reuse prefixes.

### Indexed range preparation and continuation

The new range profile owns no timer or perpetual task. Existing group recovery,
delivery, metadata and network-input owners schedule its explicit More/Limited
results. Source and receiver preparation each admit at most four pages with 250
examined rows and 32 index insertions per page: together eight pages, 2,000 scans
and at most 256 synchronous index insertions per host turn. These are operation
ceilings, not measured wall-clock latency promises. Preparation that consumes
rows returns continuation before starting the separate index phase.

Derived range indexes reserve a conservative 1,024 bytes per admitted ID against
a process-wide 32 MiB allowance (at most 32,768 IDs across all such indexes, not
per group). SQL prepared-view cache admission is reduced to 32 MiB so the original
64 MiB per-owner envelope is not silently doubled. Immutable snapshot Arc leases
retain their reservation until the last reader retires; invalidated builders reset
partial state. Capacity exhaustion is explicit limited work, never an unbounded
legacy fallback or an empty/equal summary. This in-memory tier does not claim
unlimited historical-group indexing; a future SQL-backed summary tier must honor
the same view authorization and bounded query contracts.

Validation selectors: `range_` (including real production GuardedSyncStore and
SecureChannel bidirectional/pool-refresh tests), `node::session::range_sync::tests`,
`turn_budget_tests`, `bounded_sync_retry_tests` and `delta_transition_tests`.

Legacy budget exhaustion can interrupt an old-peer exchange; that channel is
retired and follows the existing transport retry policy. It is not recycled as a
clean `Limited` range turn. New-profile `More`/`Limited` retain the explicit fair
continuation/sparse-repair behavior described above.

See [bounded synchronization verification](bounded-sync-verification.md) for the
verified runtime scope, test profile and remaining cold-index limitations.
