# Channel replica release rehearsal

The current product promise is **backup-based readable history** on a replacement
device. A new device needs a fresh channel invitation for future messages. Peer
replicas hold signed ciphertext but cannot give that device sender keys for old
channel messages missing from its backup. See [backup and recovery](backup-recovery.md).

## Repeatable local gates

Run `./scripts/channel-scale-rehearsal.sh` from any directory. It exercises
complete Nodes over real loopback TCP and Noise at 5, 20, and 50 members. The
sender exits after initial fanout; some members miss the send and later recover
from peers. It reports initial TCP contacts, encrypted application-frame bytes
sent by the origin during those contacts (including frame prefixes, excluding
handshake and any post office), send latency, and convergence time. The script
also runs the real-process UDP discovery recovery test and whole-file replica
eviction test.

One macOS loopback run on 2026-10-14 produced:

| Members | Sender contacts | Sender frame bytes | Online convergence | Missed-member recovery |
|---:|---:|---:|---:|---:|
| 5 | 4 | 3,276 | <1 ms | 102 ms |
| 20 | 2 | 2,184 | 8,930 ms | 102 ms |
| 50 | 2 | 2,184 | 5,679 ms | 207 ms |

Times depend on the host and scheduling; compare repeated runs and keep the
per-conversation `mesh_talk::sync` and `mesh_talk::channel` debug metrics. The
five-member case deliberately takes one member offline before sending. The
larger cases take unseeded members offline immediately after sending.

Run `./scripts/channel-compat-rehearsal.sh` to build commit `b1e6486`, the
last pre-replica CLI, in an isolated temporary worktree and test it against the
current CLI. A legacy message must arrive; after the current peer publishes a
replica preference, the older binary must receive neither event kind 14 nor
subsequent channel messages. Upgrade every group member before enabling the
replica toggle. The safety behavior is an interrupted sync, not seamless
mixed-version interoperability.

## Physical LAN trial before release

Run the same 5/20/50-member scenarios on separate devices on the intended LAN.
Record sender upload, time until every online member reads the message, and
time until a disconnected member recovers after the origin exits. Repeat with
loss of a seeded replica, a stale reachable replica, and enough completed files
to cross the 64 MiB channel-file replica budget. Confirm that retained files
remain downloadable and evicted files are reported unavailable when no source
remains. Run the full health gate and relevant Playwright recovery scenario on
the candidate revision.

The loopback harness does not model Wi-Fi contention, multicast filtering,
cross-device clocks, or multiple operating systems. The current anti-entropy
engine streams full event-id sets on a mismatch. Use measured `have_kib`, diff,
and round counts before adding range-based reconciliation or a gossip mesh.
