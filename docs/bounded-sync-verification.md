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

The first cold qualified preparation/index remains O(N) overall, spread across
turns. Qualified ordinary appends reuse prepared prefixes. The in-memory index
tier reserves 1,024 bytes per ID against a process-wide 32 MiB cap (32,768 total
IDs), alongside a 32 MiB SQL prepared-view cache. Pressure evicts idle derived
plans and retries on a later turn; unavailable capacity returns explicit Limited.
A SQL-backed summary tier and a remote latest-200/history-window protocol are not
implemented. The frontend already loads a 200-record history page. No seven-day
retention or filtering of authorization/key/control records is introduced.

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

File-manifest transfer retains its explicit legacy requester in the separate file
transfer lane; these group-history limits do not claim to bound that protocol.

`cargo clippy -p mesh-talk-core --all-targets --features fast-test-kdf -- -D warnings`
passed using an isolated target and the development core optimization override.

Repository hygiene and standards checks also passed (1,240 source inputs and 182
contracts checked). This is targeted synchronization verification, not a claim
that every core or desktop integration test was executed.
