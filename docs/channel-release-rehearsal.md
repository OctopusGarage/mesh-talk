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
| 5 | 4 | 2,360 | 5 ms | 103 ms |
| 20 | 2 | 2,184 | 3,130 ms | 103 ms |
| 50 | 2 | 2,184 | 6,130 ms | 222 ms |

The corresponding `./scripts/channel-scale-rehearsal.sh` output was:

```text
channel-tcp members=5 contacts=4 initial_holders=3 offline=1 sender_frame_bytes=2360 send_ms=34 online_converge_ms=5 offline_recovery_ms=103 total_ms=508
channel-tcp members=20 contacts=2 initial_holders=2 offline=3 sender_frame_bytes=2184 send_ms=32 online_converge_ms=3130 offline_recovery_ms=103 total_ms=4668
channel-tcp members=50 contacts=2 initial_holders=2 offline=10 sender_frame_bytes=2184 send_ms=51 online_converge_ms=6130 offline_recovery_ms=222 total_ms=10363
```

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

## Physical LAN rehearsal

On 2026-10-14, release CLI binaries from commit `f369c45` ran over the same
`192.168.3.0/24` LAN on the local Apple Silicon Mac, `mac2015` (Intel macOS),
and `windows10`. Each peer was a separate process. The 5-member case placed
2/2/1 peers on those hosts; 20 and 50 placed 18/1/1 and 48/1/1. The sender
discovered every peer before channel creation. For each run, the sender created
a channel, sent a message and waited for every online CLI to print it. Windows
then went offline; the sender sent another message, waited for every remaining
online member, and exited. Windows restarted with its saved identity and log
and received the missed message from a peer. All four runs completed.
An initial two-peer Noise/TCP check delivered a Mac-to-Windows DM in 172 ms
and a Windows-to-Mac DM in 232 ms after the temporary firewall rule was added.

| Members | Channel creation | First message: slowest online peer | Offline send: slowest online peer | Windows recovery after restart |
|---:|---:|---:|---:|---:|
| 5 | 108 ms | 65 ms | 152 ms | 1,071 ms |
| 20, first run | 2,827 ms | 4,999 ms | 15,212 ms | 1,185 ms |
| 20, repeat | 904 ms | 6,901 ms | 29,101 ms | 1,093 ms |
| 50 | 4,293 ms | 4,827 ms | 3,823 ms | 1,111 ms |

Timing begins when the CLI command is written; a delivery ends when the
receiver prints the decrypted channel message. Recovery timing begins just
before the Windows process restarts, so it includes startup. These are small
samples, not latency percentiles. The 20-member runs show substantial tail
variation, so this evidence does not support a fixed low-latency guarantee.
The 50-member group comprised 50 processes on three physical machines, not
50 independent devices. This rehearsal did not measure sender upload bytes or
channel-file download, inject packet loss, or evict a seeded replica. Windows
needed a temporary inbound firewall rule for the test binary; the rule was
removed afterward.

Before a release performance claim, repeat with more independent devices on
the intended LAN. Record sender upload and the `mesh_talk::sync` metrics,
investigate the 20-member tail, and test loss of a seeded replica, a stale
reachable replica, and completed files crossing the 64 MiB replica budget.
Confirm retained files remain downloadable and evicted files report
unavailability when no source remains. The current anti-entropy engine streams
full event-id sets on a mismatch; use measured `have_kib`, diff, and round
counts before adding range-based reconciliation or a gossip mesh.
