# EasyTier compatibility research for Mesh Talk

Follow-up: [two-Mac EasyTier acceptance](evals/easytier-macos-2026-10-17.md)
records the subsequent installation and successful virtual-IP messaging/file
tests. The research and its original verification limits below describe the
earlier source-investigation phase.
The EasyTier inventory adapter is now implemented; use the
[current discovery guide](routed-network-discovery.md) for supported behavior.

Research date: 2026-10-17. This is a source-based feasibility assessment, not a claim of verified cross-platform interoperability. Users install and connect EasyTier independently. No VPN service or configuration was changed during this investigation.

**Conclusion:** EasyTier is a feasible additional IP carrier for Mesh Talk's
existing discovery and authenticated transport. Manual virtual-IP discovery can
use the current code; same-/24 automatic discovery may already work. Predictable
overlay-wide automatic discovery should use a host-side EasyTier route-inventory
adapter. No live EasyTier interoperability result is claimed here.

## Upstream baseline and evidence

GitHub's latest release resolved to **v2.6.4**, commit `8428a89d2dabc94c97d370ec607c6ca142473626`, when checked. Source links below pin that commit because the development branch has since reorganized modules. Recheck the release before selecting a production support baseline. See [release](https://github.com/EasyTier/EasyTier/releases/tag/v2.6.4) and [pinned manifest](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/easytier/Cargo.toml).

## What EasyTier provides

| Capability                                                       | Upstream evidence                                                                                                                                                                                                                                                                                                  | Compatibility implication                                                                                                                                                                                                                            |
| ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| An IP overlay using an L3 TUN interface                          | `create_tun` explicitly configures `Layer::L3`; IPv4 packets read from the interface are dispatched by destination IP. [Virtual NIC implementation](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/easytier/src/instance/virtual_nic.rs#L495-L579)                             | Ordinary host TCP and UDP can use reachable overlay addresses. This is sufficient in principle for Mesh Talk's TCP transport and UDP unicast discovery, subject to application bindings, firewall and ACLs. It is not an Ethernet bridge.            |
| NAT traversal, multiple underlay transports and relayed fallback | Upstream describes UDP/IPv6 traversal, TCP/UDP/WSS/WG tunnels and shared-node relay when direct connections fail. [Pinned README](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/README.md)                                                                                    | Mesh Talk can remain unaware of the outer path. A working overlay route need not be a direct internet path. Relay performance still needs measurement.                                                                                               |
| IPv4 multicast and broadcast fan-out inside the overlay          | `is_all_peers_broadcast_ipv4` matches limited broadcast, multicast and subnet last-address; `get_msg_dst_peer_ipv4` selects remote routes having IPv4 addresses. [Peer manager](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/easytier/src/peers/peer_manager.rs#L1550-L1592) | It would be incorrect to infer that EasyTier drops multicast merely because it uses L3 TUN, or to copy Tailscale's multicast limitation. A packet must first reach the TUN interface; receiver multicast membership and OS delivery must still work. |
| Optional Windows UDP relay                                       | v2.6.4 introduced `--enable-udp-broadcast-relay`, disabled by default and requiring administrator privileges. [Release notes](https://github.com/EasyTier/EasyTier/releases/tag/v2.6.4)                                                                                                                            | This capability cannot be assumed on older installed versions or other operating systems.                                                                                                                                                            |

The Windows relay also captures outbound UDP multicast from physical-interface source addresses, preserves multicast destination addresses, rewrites the source to the virtual IPv4 address and recomputes checksums. It is compiled for Windows; its WinDivert path is architecture conditional. That makes it relevant to discovery packets emitted on the physical adapter, but it is not evidence that Mesh Talk's signed discovery address and transport advertisement remain correct after rewriting. Check the actual signed payload and socket behavior in a real test. [Relay parser, normalizer and capture filter](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/easytier/src/instance/windows_udp_broadcast.rs#L233-L349).

## Obtaining candidates from an existing client

Use route inventory rather than public underlay endpoints. EasyTier exposes `ListRoute`, whose entries include virtual IPv4/IPv6, peer ID, next hop, cost and hostname. A relayed route remains useful; a node without a virtual IP is not a host candidate. Never expand advertised `proxy_cidrs` into arbitrary subnet scans. [RPC schema](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/easytier/src/proto/api_instance.proto#L64-L133).

For a CLI adapter, start with `easytier-cli -p 127.0.0.1:15888 -o json route`. In v2.6.4 this emits presentation rows with `ipv4` strings and includes the local row; verbose output instead emits raw `node_info` and `peer_routes`. Multi-instance selection can wrap results differently. Parse a narrow, bounded schema, ignore self and empty addresses, and maintain fixtures by supported version. The IPv4 route table output does not establish IPv6 candidate extraction; use the raw schema or a separately verified IPv6 command for that. [CLI options, instance-result JSON and route formatting](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/easytier/src/easytier-cli.rs#L1700-L1840).

The CLI defaults to contacting `127.0.0.1:15888`, but **the v2.6.4 core does not default to loopback binding**: without an explicit address it selects an available port in `15888..15900` on `0.0.0.0`. Explicitly bind the management endpoint to loopback. GUI/service instances may use another endpoint, so a missing default endpoint must be a nonfatal adapter-unavailable state. [RPC binding implementation](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/easytier/src/rpc_service/api.rs#L178-L214).

## Reproducible configuration proposal

The following is an unexecuted two-host starting configuration, based on the upstream TOML parser test. Choose an overlay subnet that does not overlap either host's existing routes. Protect these files because the network secret is stored in them. Replace the example secret with the same strong random secret on both hosts. [Configuration example and parser test](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/easytier/src/common/config.rs#L1450-L1480).

Host A (`a.toml`):

```toml
instance_name = "mesh-talk-easytier-test"
ipv4 = "10.144.144.1/24"
listeners = ["tcp://0.0.0.0:11010", "udp://0.0.0.0:11010"]

[network_identity]
network_name = "mesh-talk-private-test"
network_secret = "REPLACE_WITH_A_STRONG_SHARED_SECRET"
```

Host B (`b.toml`) uses `10.144.144.2/24`, the same network identity, and adds a reachable bootstrap endpoint:

```toml
[[peer]]
uri = "tcp://HOST_A_REACHABLE_ADDRESS:11010"
```

Run each EasyTier core with its file and an explicitly local management endpoint:

```sh
sudo easytier-core --config-file a.toml --rpc-portal 127.0.0.1:15888
# On Host B, use b.toml instead.
easytier-cli -p 127.0.0.1:15888 -o json route
ping 10.144.144.2
```

The core supports `--config-file` and `--rpc-portal`. This example requires Host A to be reachable at the configured outer TCP endpoint; if both hosts are behind restrictive NAT, select an independently operated reachable bootstrap/relay instead. Do not enable `--no-tun` for this host-socket compatibility test. Preserve existing firewall policy and allow only the actual overlay discovery/transport traffic required by Mesh Talk. [Core CLI definitions](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/easytier/src/core.rs).

## Platform and integration scope

Upstream documents Windows, macOS, Linux, FreeBSD and Android support. Its normal host-network examples run with administrator/root privileges; an Android VPN service or a WireGuard portal is a separate deployment shape, not proof that the desktop application works there. Windows release packages also contain driver/runtime components. Treat supported Mesh Talk desktop combinations as a smaller matrix requiring explicit verification. [Upstream platform and installation guidance](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/README.md).

EasyTier is LGPL-3.0. Prefer compatibility with an independently installed process first; vendoring or linking its Rust library creates a larger packaging and lifecycle decision. No legal conclusion about a combined distribution is made here. [Pinned license](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/LICENSE).

## Limits of this investigation

This upstream assessment inspected release source and official repository material. It did not create a tunnel, change routing or firewall configuration, start privileged services, validate multicast delivery across two hosts, measure relay throughput, or establish WebRTC call compatibility. Those are acceptance tests before advertising support.

## Mesh Talk code assessment

This section examines the working tree on 2026-10-17, based on commit
`b59c2952cc293b64f6c3ad57c5b048caf52c5c95` plus pre-existing local changes.
It describes the inspected files, not a claim about a published Mesh Talk binary.

### What already works at the architecture boundary

Mesh Talk's transport is not tied to Tailscale. The
[`CandidateSource`](../crates/mesh-talk-core/src/discovery/candidates.rs)
interface yields generic socket addresses. The
[participant runtime](../crates/mesh-talk-core/src/node/participant_runtime.rs)
requests a snapshot every 25 seconds or on rescan, applies a three-second
snapshot timeout, deduplicates and caps it at 256 endpoints, then probes through
the existing routed-discovery path. The
[desktop bootstrap](../src-tauri/src/runtime_bootstrap.rs) currently installs
only [`TailscaleCandidates`](../src-tauri/src/tailscale_discovery.rs).
Consequently, **the transport can use an EasyTier OS route, but a dedicated
EasyTier inventory adapter is not implemented.**

The [UDP discovery service](../crates/mesh-talk-core/src/discovery/service.rs)
sends nonce-bearing identity-free queries and verifies signed replies before
recording a routed endpoint; the normal Noise transport then authenticates
application identity. An EasyTier route must remain an untrusted address
candidate: neither VPN membership nor the peer's virtual IP grants contact,
account, group, or relay authority. See the
[current routed-discovery contract](routed-network-discovery.md) and
[architecture](ARCHITECTURE.md).

[Socket binding](../crates/mesh-talk-core/src/transport/net.rs) already accepts
IPv4 and, where available, IPv6 traffic on wildcard listeners. The desktop
[`stable_listener`](../crates/mesh-talk-core/src/node/privacy_routes.rs)
persists a selected TCP port and reuses it when available, selecting another if
necessary. Opening only UDP 47474 is insufficient: the advertised TCP listener
must also be reachable. The CLI can request a fixed listener with `--port`.

### Automatic discovery has a useful but limited existing fallback

Do not conclude that EasyTier always needs a new adapter before any discovery
can work. In normal public discovery mode, the
[socket helpers](../crates/mesh-talk-core/src/transport/net.rs) join
`224.0.0.167` on all enumerated non-loopback IPv4 interfaces. The
[scan fallback](../crates/mesh-talk-core/src/discovery/service.rs)
also enumerates those interfaces and sends signed announcements to addresses
`.1` through `.254` in each interface address's /24, excluding local
addresses and the `100.64.0.0/10` range. It does not use the interface's actual
netmask in unrestricted mode.

Therefore, with EasyTier virtual addresses such as `10.144.144.1` and
`10.144.144.2`, both peers may discover each other through existing unicast
scanning, even if multicast takes the wrong OS route. This is **source-based
feasibility, not an observed EasyTier test result**. Cross-/24 peers,
IPv6-only peers, excluded CGNAT addresses, host enumeration differences,
visibility settings, and firewall rules can prevent this fallback. An address
ending in `.0` or `.255` is also not covered by the scan. Multicast working
at the EasyTier layer does not prove the host routes Mesh Talk's outgoing
multicast into it.

A related privacy detail: this fallback sends the signed application
announcement across the scanned /24. It is not the identity-free query used by
the routed candidate path. Do not broaden subnet scanning as the integration
strategy. Explicit route inventory is more targeted.

The physical LAN classifier in
[`scope.rs`](../crates/mesh-talk-core/src/discovery/scope.rs)
conservatively rejects point-to-point and common virtual adapters. This does
**not** mean normal public discovery always excludes TUN interfaces: the
physical restriction is activated by `physical_lan_only`.
[`DiscoveryVisibility`](../crates/mesh-talk-core/src/discovery/visibility.rs)
suppresses routed probing and off-LAN replies under that setting; invisible
mode also blocks public discovery. An EasyTier peer should not become a
physical-LAN member merely because its virtual address is private.

### A minimal trial using current Mesh Talk

After users establish a normal EasyTier TUN network externally:

1. Confirm each endpoint has a virtual IP and a permitted bidirectional OS
   route. For the first trial use non-overlapping IPv4 ranges.
2. Run compatible Mesh Talk nodes. Enable public visibility and leave
   **Physical LAN discovery only** off for this trial. Keep normal application
   admission rules in place.
3. Permit discovery UDP 47474 and each node's advertised TCP listener in both
   EasyTier policy and the host firewall. Distinguish those inner application
   ports from EasyTier's outer transport ports and local management RPC.
4. Try the existing rescan. If it does not find the peer, use the desktop
   connection dialog with the peer's **virtual IP only**, for example
   `10.144.144.2`. The
   [current UI](../frontend/src/features/chat/OfflineConnectDialog.tsx) calls
   [the diagnostics API](../frontend/src/lib/api/diagnostics.ts) with a host,
   defaulting to discovery port 47474. It does not expose a custom port field.
   The [backend](../src-tauri/src/chat_commands/diagnostics.rs) supports an
   optional port, but rejects a host string containing `IPv4:port`.
   For a custom discovery port use the CLI, e.g. `/probe 10.144.144.2:47475`.
5. Verify the Mesh Talk fingerprint, exchange messages in both directions, and
   transfer a file. A successful probe submission only confirms the query was
   queued; require a verified peer and actual delivered content.

No new application wire protocol or EasyTier SDK is needed for this trial.
This procedure remains unverified over a live EasyTier network.

### Recommended implementation scope

For predictable automatic discovery, add a host-owned `EasyTierCandidates`
adapter and compose it with Tailscale behind the existing `CandidateSource`
interface. Keep all provider-specific commands and schemas outside core.

- Read the selected local EasyTier instance's route inventory, including peers
  reachable through intermediate EasyTier nodes. A direct-neighbor peer list is
  insufficient for the full overlay.
- Handle documented and tested CLI versions explicitly. Use JSON, not terminal
  tables; distinguish presentation rows from verbose/raw route output and
  multi-instance wrappers.
- Exclude self and entries without a usable virtual address; deduplicate and
  retain provenance. Feed candidates to the existing signed query rather than
  importing VPN peers directly into the roster.
- Bound subprocess duration and output, kill on cancellation, and return an
  empty snapshot when the CLI, permissions, RPC instance, or supported schema
  is unavailable. Apply an aggregate deadline when composing providers so one
  unavailable provider does not consume the entire three-second snapshot
  budget and starve the other.
- Support an explicit instance/RPC endpoint and executable location where
  needed. A packaged GUI process may have a different PATH from an interactive
  terminal. Do not launch, reconfigure, enroll, or elevate EasyTier as part of
  candidate collection.
- Preserve visibility and physical-LAN restrictions. Keep multiple IP routes
  for one Mesh Talk identity; switching VPN providers must not create a new
  person/device identity.

This is a proposal, not an implementation made during this research.

### Capability and acceptance boundaries

| Capability                            | Assessment                                                            | Evidence still needed                                                                     |
| ------------------------------------- | --------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Direct messages, group events, files  | Compatible in principle through reachable virtual-IP Noise/TCP routes | Two real hosts on separate physical networks, bidirectional delivery and file hash        |
| Existing same-/24 automatic discovery | May already work through fallback scan                                | Actual host interface enumeration and packet delivery                                     |
| General automatic EasyTier discovery  | Needs an inventory adapter for predictable coverage                   | Old/current CLI fixtures, custom RPC port, multiple instances, multihop peers, absent CLI |
| Post-office and bridge operation      | Existing routed endpoint model can use reachable EasyTier addresses   | Explicit bridge opt-in, offline delivery, relay failure, and federation topology trials   |
| Linked-device synchronization         | Underlying reachable route is compatible in principle                 | Actual linked-device replay and route changes                                             |
| Voice/video calls                     | Separate acceptance item                                              | Real webview ICE gathering and selected media path on macOS, Windows, Linux               |

The [call implementation](../frontend/src/store/calls.ts) uses
`RTCPeerConnection({ iceServers: [] })`, with host candidates only. TCP chat
success does not show that the webview exposes the EasyTier virtual interface
to ICE or can establish media on it. Neither EasyTier's relay nor Mesh Talk's
post office supplies a WebRTC TURN service by implication.

Before advertising EasyTier support, test direct and EasyTier-relayed paths,
cross-/24 and multihop peers, DHCP/IP changes, EasyTier reconnects, host
firewalls, and concurrent LAN/Tailscale/EasyTier routes. Include physical-LAN
restriction and invisibility checks, provider absence/failure, and duplicate
identity handling. An EasyTier-only device does not automatically reach a
Tailscale-only device: they need a shared routable path or separately configured
Mesh Talk relay topology. A VPN endpoint likewise does not expose its entire
physical LAN without explicit subnet routing or application bridge setup.

## Local verification performed

Read-only checks found installed `easytier-cli` and `easytier-core`, both
reporting `2.4.3-445e68dd`. Their help confirms JSON output and configurable
RPC targeting. A read-only `easytier-cli -o json route` request failed because
`127.0.0.1:15888` refused the connection. This establishes only that the
default RPC endpoint was unavailable; a GUI or another instance could use a
different endpoint. No daemon was started and no network settings were changed.

The following existing focused tests were run against the inspected Mesh Talk
working tree; **each passed one test with zero failures**:

```sh
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=runtime_automatically_queries_a_host_supplied_routed_candidate
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=runtime_manual_endpoint_probe_uses_the_same_query_protocol
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=physical_scope_blocks_off_lan_queries_and_raw_announces_then_allows_live_opt_out
```

These prove the exercised generic candidate, manual query, and disclosure-gate
paths on local loopback. They do not prove EasyTier interoperability,
cross-platform behavior, file/media performance, or any live relay topology.
No full workspace suite or E2E suite was run for this research-only change.
