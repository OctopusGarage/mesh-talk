# Bounded group synchronization verification

The production entrypoint is `request_sync_turn_paged`. New group peers use an
authorized outgoing range index, bounded exact body requests and reverse-role
exchange. Old peers use a separately bounded legacy compatibility turn. Explicit
`request_round_paged` remains a legacy drain API for callers that request it.

A new-profile turn admits at most 200 records and 1 MiB encoded bodies across both
directions, with eight synchronization RPCs. Exact durable-custody confirmation
is a separate bounded request. Source and receiver work each admit four pages,
32 insertions and 250 examined rows per page. Operation counts are ceilings, not
wall-clock latency guarantees.

The first cold qualified authorization preparation remains O(N) overall, spread
across bounded turns. Qualified ordinary appends reuse prepared prefixes. The
production full-history index now uses the existing encrypted disk-spilling
participant scratch pager; the former 32 MiB/32,768-ID RAM admission ceiling no
longer applies to production indexes. The bounded prepared-view cache remains.

The negotiated recent-history profile returns at most 200 selected Message
records, further bounded by 60 KiB of encoded event bodies and the transport
frame. Older pages require an explicit opaque continuation. The foreground node
API and desktop group-history action use it; page coverage remains separate from
full repair. No seven-day retention or filtering of authorization/key/control
records is introduced.

Recent records still obey strict causal admission. A page whose external parents
are missing returns Blocked and an explicit gap-navigation continuation; that
continuation is not a durable acknowledgement for rejected bodies. Once needed
parents arrive through older pages or existing bounded repair, a fresh page can
commit. This does not promise instant cold recent rendering without ancestor
closure. SQL scratch is derived and requalified after process restart, not a
persistent cache of permissions.

`More` preserves local commit reports and uses fair continuation without failure
backoff. `Limited` parks without equality or custody claims. Interrupted legacy
exchanges retire their channel and retain the existing transport retry policy.

## Verification

Targeted tests use the ordinary Rust test harness with `fast-test-kdf`, an
isolated target directory, test debug information disabled, and
`--config 'profile.test.package.mesh-talk-core.opt-level=0'`. This avoids the
workspace's optimized core test compile during development; production behavior
is not changed. The macOS linker reports its existing large `__eh_frame` compact
unwind warning in the test binary.

- `node::session`: 130 passed, one ignored. Covers compatibility, bounded paged
  selection, metadata/control gates, transport limits and custody semantics.
- `range_`: 15 passed, including real GuardedSyncStore/SecureChannel bidirectional
  convergence, pooled snapshot refresh and ongoing appends between More turns,
  authorized prefix reuse and cold preparation retained across reconnection.
- `eventlog::sync::range`: four passed, including atomic page reservation retry,
  invalid source progress and scope/snapshot rejection.
- `pressure_retirement_is_oldest_first_and_updates_byte_accounting`: one passed.
- `targeted_group_export_checks_selection_and_bounds_bodies`: one passed,
  preserving qualified selection, unavailable targets and oversized-body progress.
- `delta_transition_tests`: two passed for pinned mailbox windows and duplicate/
  nonadvancing cursor rejection.
- `bounded_sync_retry_tests`: three passed. Verifies More/Limited scheduling does
  not claim delivery or increment transport failure backoff.

The existing
`late_initial_receive_keys_repair_an_already_present_message_through_history`
regression also passed. Its fixture starts account history/wake workers, so it
explicitly drives bounded group-distribution continuations to model the separate
group recovery owner. The initial-key history gate stays closed while ciphertext
and later signed key records arrive; authorization, later key position, absence
of premature plaintext and eventual history repair assertions remain intact.
Local account maintenance settling a signed publication is separate from network
fanout completion: the existing group recovery queue retains that delivery work.

The range primitive is adapted from Mesh Foundation commit
`4bf8f92269d81b8f20e0f807e04d7a7cf6fe2588`; the application uses an imperative
checkpoint adapter for Send-compatible network futures and blocked-frontier
scheduling. No new package release is part of this application change.

Explicit direct DM exchanges for controls and attachment manifests use the
bounded SDK legacy turn. The existing direct attachment recovery test verifies
the manifest arrives before exact signed blocks are requested. The separate
attachment-content lane keeps its dedicated file requester.

`cargo clippy -p mesh-talk-core --all-targets --features fast-test-kdf -- -D warnings`
passed using an isolated target and the development core optimization override.

Repository hygiene and standards checks also passed (1,240 source inputs and 182
contracts checked). This is targeted synchronization verification, not a claim
that every core or desktop integration test was executed.

## Recent-history and stored-range follow-up

The recent profile limits a page to 200 messages, 60 KiB and 1,000 examined
rows; a foreground request makes at most four exchanges and never drains older
continuations automatically. Source-pinned descending keyset pages preserve the
qualified snapshot. Blocked pages expose navigation gaps without acknowledging
rejected child records. The dialog keeps its separate 500-message display limit.

The derived range tree now uses the encrypted scratch SQLite pager. Retained
heap has a hard 32 MiB budget; derived SQL storage has a 512 MiB soft LRU quota
with one oversized plan admitted for progress. Per-selection conservative charge
counters make cache admission O(1), without scanning the cached tree each turn.
Failed builds reuse their namespace after bounded retirement. Navigation and
blocked frontiers stop at 4,096 entries rather than growing without bound.

Negotiated v2 ordered repair seeks ascending source-rank windows near at most
32 missing-parent anchors, preserving strict admission and ordinary turn limits.
An exhausted repair page resets Merkle verification; it cannot directly claim
whole-group equality. The existing protocol remains available to older peers.

The current SQL/index regressions exercise 32,769 IDs, canonical digest equality,
rollback, source-rank seeks, failed-builder retirement, cache charge accounting,
source-pinned 200+1 pages and active progress when idle caching is declined.
Cold authorization can still examine O(N) headers in bounded units; derived
permission selections are reconstructed after restart. No production latency
or throughput claim is made.

The additional focused run passed 24 distinct tests (25 filter executions, with
SQL charge cases intentionally overlapping): keyset/anchor queries, recent wire
and real two-node page behavior, SQL tree/reset, charge/retirement accounting,
bounded frontiers and source/receiver incremental reuse. The 32,769-ID case
passed in the unoptimized isolated test profile; that duration is not a benchmark.
Frontend history tests passed five cases and TypeScript checking passed.

A real two-node regression uses a 201-message parent chain, concurrent appends on
successive bounded turns and a later pooled-connection refresh. Its longer test
continuation allowance accommodates cold authorization replay of the newly
created tail; runtime budgets are unchanged. The observed reducer cursor advances
through the finite source rather than restarting. Ordered forward repair defers
reverse preparation until its dependency closure exits, then the same test
verifies transfer in both directions. Existing qualified SQL views and completed
receiver snapshots can continue without reconstructing an obsolete principal;
policy/destructive fences are still checked. An unreconstructible old principal
invalidates append reuse and starts qualification again; storage errors propagate.
A separate live-lease test covers idempotent rebinding of the same SQL index and
rejection of a different selection/lane while the original snapshot remains valid.

Final clean-source checks passed: the complete `node::session::` unit filter
passed 111 tests with the existing scale rehearsal ignored; the late-initial-key
history repair regression passed; core all-target Clippy denied warnings; and
`cargo +1.99.0 check -p mesh-talk --lib` passed. Repository gates checked 1,241
source inputs and 183 contracts, plus formatting and staged source hygiene.
These checks do not claim the ignored scale rehearsal or all desktop integration
scenarios were executed.
The clean-source long-chain production regression also passed three randomized
runs, alongside the live-snapshot SQL rebinding regression.
