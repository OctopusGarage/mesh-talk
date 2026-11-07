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

An existing target miss requires recorded baseline and an owned follow-up; it is
not permission to worsen the path or silently raise its budget. A target change
requires comparable evidence and review. Migrations, catch-up and active media have
separate bounded work budgets and progress evidence, not an unlimited exception.

## Design and review rules

1. **Trigger work from meaning.** Prefer committed changes, scoped invalidations,
   user navigation and foreground activation. Schedule the next actual expiry or
   retry deadline. Empty success is not progress and must not keep a fast retry
   hot. Coalesce duplicate wakes; retain bounded dirty work while a task is busy.
2. **Use recovery deliberately.** A fallback repairs missed events; it is not the
   main data path. Back off failures, stop with the owner and refresh on return.
   Native delivery, discovery leases, calls and real expiry retain their own
   timing contracts; never slow them merely to meet an idle chart.
3. **Read metadata before payloads.** Compare revision/digest/cursor and query
   pending keys before reading, decrypting, cloning or decoding bodies. Paginate
   changes; drain available pages promptly in bounded batches. A slower fallback
   must not delay each already-available page by another interval.
4. **Bound every retained layer.** Budget bytes/pixels as well as entry count,
   concurrency and queues. Virtualized DOM is not a bounded data store. Keep a
   small active window; evict inactive payloads and load originals only on demand.
   Compressed file size is not decoded image memory. Avoid keeping archives,
   base64 strings, Blobs and decoded originals together without a measured need.
5. **Write only durable changes.** No-op reads/heartbeats must not rewrite logs,
   snapshots or indexes. Batch safe related mutations under the existing durable
   transaction; atomically update derived indexes. Bound WAL/log maintenance;
   preserve crash recovery and acknowledgment semantics when reducing flushes.
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

## Evidence and maintenance

Before optimizing, identify the owner and capture the offending operation count
or allocation/CPU/write trace. Add a regression that fails on that mechanism,
then validate empty/no-change, growth, failure/retry, cancellation and recovery.
Compare the same device/build/runtime/profile; include whole-family resource
measurements and a correctness witness. Heap snapshots/retaining paths distinguish
leaks from stable bloat and allocation churn. Forced working-set trims, periodic
GC, cache deletion or moving pressure to disk do not prove a fix.

When a new defect exposes a reusable pattern, extend the relevant rule here and
its owning capability/test selector. Keep each addition to mechanism, requirement
and regression reference; do not append incident histories, benchmark diaries or
duplicate feature contracts. Follow the [validation protocol](testing-feedback-loop.md).

Primary references: [WebView2 performance](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/performance),
[Edge memory diagnosis](https://learn.microsoft.com/en-us/microsoft-edge/devtools/memory-problems/),
[IndexedDB clone semantics](https://developer.mozilla.org/en-US/docs/Web/API/IDBObjectStore/getAll),
[SQLite WAL](https://www.sqlite.org/wal.html),
[SQLite reclamation](https://www.sqlite.org/lang_vacuum.html),
[Windows power efficiency](https://learn.microsoft.com/en-ca/windows/apps/performance/power),
[Apple timer guidance](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html),
[Matrix incremental sync](https://spec.matrix.org/v1.19/client-server-api/#syncing),
[TDLib cache reclamation](https://core.telegram.org/tdlib/docs/classtd_1_1td__api_1_1optimize_storage.html).
