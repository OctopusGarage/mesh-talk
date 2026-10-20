# Routed discovery and cross-network setup

Mesh Talk discovers compatible application endpoints across an existing permitted
IP route, including Tailscale and EasyTier. Mesh Talk never installs, configures
or authorizes either VPN or its accounts/devices. It does not change VPN ACLs,
merge LANs or trust a device because it appears in a VPN inventory.

For communication principles, module contracts and illustrated cases, use the
[network architecture baseline](network-design-reference.md). This guide owns
candidate discovery and setup; [federation operations](post-office-federation.md)
own equal-relay enrollment/deployment.

## Boundaries

The desktop automatically reads both optional local-client inventories:

| Provider  | Inventory                                                                 | Candidate addresses                                                                                                                        |
| --------- | ------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Tailscale | `tailscale status --json`                                                 | Visible online peer addresses when the client is running; self is omitted.                                                                 |
| EasyTier  | `easytier-cli --rpc-portal 127.0.0.1:15888 --verbose --output json route` | Virtual IPv4/IPv6 addresses of direct and multihop routes; self, unusable addresses, proxy subnets and outer tunnel endpoints are omitted. |

Both feed the same core `CandidateSource` interface. Collection runs concurrently
with independent 2.5-second deadlines, below the core's three-second snapshot
budget. Results are interleaved, deduplicated, and capped at 256 total UDP endpoints
so neither provider takes the entire candidate budget. Missing clients, denied
RPC access, timeouts or incompatible JSON make that provider unavailable without
discarding the other's results. Discovery starts automatically and refreshes
every five seconds; **Connection → Rescan** also refreshes both inventories.

EasyTier JSON parsing is verified against CLI 2.6.4. Multiple network-instance
results from the selected management endpoint are supported. A node on a
different management endpoint requires `MESH_TALK_EASYTIER_RPC`, for example
`127.0.0.1:15899`; only loopback IP endpoints are accepted. To select a CLI outside
the normal PATH/install locations, set `MESH_TALK_EASYTIER_CLI` to its executable
path in the app's launch environment. macOS lookup includes user-local, Cargo,
Homebrew and the installed MeshTalk-EasyTier service paths. Windows lookup also
checks Program Files/EasyTier and the application's executable directory;
nonstandard ZIP locations can use the override. GUI launch environments may
have a different PATH from terminals.

The EasyTier search has a two-second total deadline and a 1 MiB output cap;
cancellation kills a running CLI process. Raw verbose output can contain private
configuration, so it is never logged or persisted. Only route fields are decoded.
These inventories are best effort, not proof of application capability.

A candidate receives a rate-limited UDP query. Only a fresh valid signed response
establishes Mesh Talk identity and a possible TCP listener. Actual data transport
rechecks pinned identity and admission through Noise. Discovery send success,
VPN presence and TCP connectivity are different observations.

## Protocol and routes

The routed query (`MTDQ`, version 1) is a fixed 64-byte identity-free UDP packet
with a random 16-byte nonce. A public responder signs the `MTDR` response, binding
that nonce to its signed announcement. The requester admits one response from an
outstanding candidate IP and nonce within five seconds after validating both
signatures. Separate nonces for services on the same IP remain independently
pending, with at most 256 pending requests; consuming one does not invalidate
another service's reply.
Invisible mode and physical-LAN-only disclosure suppress routed probing/replies.

Roster routes retain origin, freshness and authentication observations separately
from device identity. A known direct route may be LAN, routed-query or
authenticated; a gateway/mailbox hint alone is not direct reachability. Transport
tries bounded candidate routes and keeps current signed ports ahead of stale
cached ports. IPv4/IPv6 support follows available sockets and interface scope.

## Network changes and recovery

All desktop, CLI and post-office discovery sockets observe non-loopback interface
names, addresses and interface indexes every two seconds. A changed inventory
rejoins LAN multicast and wakes the existing announce/scan producers; a desktop's
shared trigger also refreshes routed candidates. Inventory ordering and duplicates
do not create changes, and an enumeration failure retains the last observation.
The 30-second periodic multicast join remains a retry fallback. Every advertisement
and routed probe still passes the current visibility/scope gates.

Fresh signed announcements supply current TCP endpoints. Changed local interfaces
cancel stale delivery control batches and mailbox pulls; changed endpoints for an
active recipient or eligible relay cancel the old control batch before retrying
its immutable queue. Unrelated peers and refresh timestamps do not cause resets.
Transport errors retire failed pooled sessions. The 15-second complete-exchange
budget remains available on stable high-latency routes. The two-second observation period and per-exchange
timeouts are scheduling bounds, not an end-to-end delivery SLA. Recovery still
requires an allowed IP path, fresh discoverable/pinned endpoints and retained
message state. A configured federation seed whose IP actually changes may need
its explicit endpoint updated if neither LAN discovery nor another retained route
can find that identity.

`scripts/e2e-network-handoff.py` moves disposable Linux container interfaces between
two real Docker bridge networks without restarting the node processes or manually
probing/rescanning. It checks queued/new messages, duplicate-free history and
recipient-authenticated receipt convergence in both directions of the handoff,
plus recovery from a relocated post office while the sender is disconnected.
The relay phase explicitly verifies the disposable contacts before enabling
proactive mailbox recovery; discovery never substitutes for verification.
Its 15-second fixture acceptance budget starts after the new route is attached.
This is network-namespace acceptance, not a physical Wi-Fi/hotspot or native
WebView latency measurement. Build Linux `mesh-talk-node` and the
`network-acceptance` example with `fast-test-kdf` into a Docker volume, then run:

```bash
python3 scripts/e2e-network-handoff.py --image <linux-runtime-image> \
  --binaries-volume <docker-volume> --binaries-subdir <directory-containing-binaries>
```

## Connect two apps directly

1. Establish the actual permitted IP route outside Mesh Talk.
2. Run compatible Mesh Talk nodes and allow the target discovery UDP port plus
   its advertised TCP listener through the relevant ACL/firewall.
3. In **Settings → Connection & delivery**, select **Device discovery scope →
   Local and routed networks**. Automatic discovery uses available local VPN
   clients; **Connection → Rescan** refreshes their inventories. If automatic
   discovery is unavailable, choose **Add device address…** and enter the other
   device's IP address or hostname. The CLI uses `/probe <ip:discovery-port>`.
4. Validate the discovered identity; normal private-contact/group permissions
   still apply. A blocked TCP listener can prevent delivery after discovery.

The managed discovery contract is platform-independent: desktops use UDP 47474;
every post office uses dedicated UDP 47478 and also participates on shared LAN
UDP 47474. The macOS and Windows service installers enforce these ports.
Desktop inventories probe both role ports on each candidate host. Both share
the same interleaved 256-endpoint budget; duplicate endpoints are queried once
per snapshot. The port numbers identify entry points; only signed responses
establish service identity and advertise its TCP port: managed post offices use
TCP 47479; desktop clients automatically choose and attempt to reuse their port.
VPN inventories advertise hosts, not Mesh Talk service ports. The raw headless
CLI retains custom discovery ports for isolated deployments/tests, which must
supply explicit discovery endpoints; they are not automatically discovered by
the desktop inventory. The current desktop manual-address form supplies the
default port only, while CLI `/probe <ip:discovery-port>` supports custom ports.
Production services must retain the managed role ports. A device-to-device VPN edge exposes those
endpoints, not all machines on their neighboring LANs. Tailscale-only and
EasyTier-only devices still need a shared permitted route or a separately
configured Mesh Talk relay topology.

Use `scripts/e2e-easytier-macos.py` for opt-in two-Mac acceptance of automatic
inventory discovery and bidirectional encrypted DM/group/file exchange. This
scenario does not establish WebRTC media, every operating system, or internet
NAT/relay behavior. Record exact source and observations in the PR or CI artifacts;
do not retain another dated implementation report in the documentation tree.

## Opt-in post-office bridge

For example, A/B share LAN X with relay E; C is on LAN Y and has an existing
permitted Tailscale connection to E. A/B need no Tailscale connection: their
physical LAN already provides direct reachability to E.

E runs `--post-office --bridge` with its persistent identity/password file and a
permitted TCP listener. A, B and C each explicitly enable **Use post offices** and
**Discover peers through a post office**, select **Local and routed networks**
for **Device discovery scope**, and permit the required mailbox directions.
Bridge discovery defaults to off; enabling E alone does not opt its clients in.

E introduces eligible registered clients using self-signed, freshness- and
lifetime-bounded presence proofs. A and B can discover C through E's authenticated
directory, and C can discover A/B. C joins this Mesh Talk discovery/mailbox
network, not LAN X's IP subnet. If A and C cannot connect directly, their encrypted
DMs, signed consent, file chunks and return controls use E's opaque mailboxes.
Directory entries do not substitute E's address for C's listener or grant group
membership. A direct route remains preferable when available.

Invisible nodes do not participate in public bridge discovery. Disabling bridge
discovery stops the client's local bridge participation and clears its bridge
introductions; it cannot erase a proof another peer already received. Remote
copies remain subject to freshness/expiry. Mailbox sending and receiving have
separate controls; turning off discovery is not a promise of remote data deletion.

The preferred-relay picker includes directly reachable verified LAN/Tailscale
candidates, including E for C. A relay learned only through another relay is not
an eligible direct preference. A saved unavailable preference is preserved;
selection can fall back to another eligible relay. Cross-site continuity still
needs a surviving onward route. Calls require live direct signaling and separate
WebRTC media; full sibling replay has its own authenticated path. With E down and
no alternate route, discovery/delivery across these isolated sites cannot proceed.

## Two LANs with one post office on each LAN

A/B use E on LAN X; C/D use F on LAN Y. Only E↔F needs an existing permitted
Tailscale route. Both relays run the same equal bridge/federation role, with unique
persistent identities and a shared public `--federation-config` manifest (or
matching inline `--federation-peer` enrollment). A client does not pin a remote
hub ID. E/F mutually authenticate enrolled identities over Noise before exchanging
eligible signed presence and version-3 opaque mailbox events.

A/B explicitly opt into directory discovery/mailbox directions through directly
reachable E; C/D do the same through F. E introduces eligible C/D proofs to A/B,
and F introduces A/B to C/D. These introductions do not make a remote private IP
directly reachable. A message can follow A→E→F→C, with ciphertext protected for
the recipient end to end and each transport hop separately authenticated. This
is not one end-to-end Noise connection through both mailboxes. Signed consent,
verified-save completion and return delivery evidence can follow the reverse
mailbox route without direct A↔C overlap.

Clients use eligible direct relays, honor an optional reachable preference and
fall back when necessary. A relay reached solely through federation is not a
client's direct relay choice. E/F may use identical TCP port numbers on different
machines; processes sharing one address must use nonconflicting ports. Shared
role/configuration does not mean sharing private keys, passwords or data files.

Version-3 mailbox reconciliation is opportunistic and bounded by trust,
reachability, policy, capacity and expiry. It does not promise that every relay
holds every queued event or guarantee replicated custody. Legacy relay-specific
mailboxes do not replicate. A surviving client-to-relay and relay-to-relay route
is required for cross-site delivery; alternate relays without an onward path
cannot provide it. Transport/custody success is not recipient confirmation.

The older `--bridge-peer` TCP gateway remains a separate compatibility path. It
requires explicitly pinned bridge peers, fresh signed remote relay proof and
end-to-end Noise authentication through the forwarded stream. The gateway itself
does not replicate stores. Its configuration and historical test results must not
be substituted for the equal-federation model or its evidence.

## Two-Mac tailnet test and other focused checks

The opt-in `scripts/e2e-tailnet-macos.py <ssh-host>` rig checks direct signed
routed discovery, bidirectional encrypted DMs and consented file SHA-256 over an
existing two-Mac tailnet. Its header owns build prerequisites and cleanup.
`scripts/e2e-four-machine-tailnet.py` separately supports federation modes;
`scripts/e2e-federation-recovery.py` tests an actual surviving alternate chain and
partition recovery. [Script entry points](../scripts/README.md) distinguishes
these scenarios and the older gateway rigs.

The [dated physical recovery record (2026-10-16)](network-design-reference.md#worked-example-current-mac-windows-mac2015-and-mac-mini)
documents four disposable equal relays across two physical sites. After the
preferred relay stops, encrypted mail traverses the surviving R→Q→S chain and
returns recipient confirmation. A 130,000-byte file survives sender restart
before consent, saves with matching SHA-256 and returns completion; stopping and
restarting Q verifies partition recovery. This is an existing observation, not a
new run. Three independent sites, a separate client on the mini's LAN and native
macOS WebView calls remain unverified.

For one shared relay across isolated discovery scopes, select the specific
`bridge_introduces_both_lans_and_delivers_both_directions` test in
`crates/mesh-talk-core/tests/bridge_cross_lan.rs`. Use the
[testing workflow](testing-feedback-loop.md) rather than automatically running
all E2E during development.

## Limits and release checks

- No Tailscale account creation, ACL changes, address-range scanning or trust based
  on Tailscale identity. Direct versus DERP transport stays below Mesh Talk.
- Firewalls, unavailable inventory, custom ports, invisible/physical-only settings
  and old incompatible nodes can prevent first contact.
- Text, files, group recovery, calls and sibling replay require distinct evidence.
  Current dated observations and remaining platform/site boundaries are in the
  [evidence tracker](network-design-reference.md#implementation-and-evidence-tracker).
- Test each claimed platform against route changes, blocked UDP/TCP, spoof/replay
  attempts and privacy transitions. Loopback discovery scopes do not prove an
  independent physical site or forced DERP path.

External references: [Tailscale CLI](https://tailscale.com/docs/reference/tailscale-cli),
[status representation](https://github.com/tailscale/tailscale/blob/main/ipn/ipnstate/ipnstate.go),
[connection types](https://tailscale.com/docs/reference/connection-types).
