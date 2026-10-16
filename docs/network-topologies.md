# Network topologies: LAN, routed peers, and post offices

This guide maps direct, routed, and federated post-office paths. **Seeing a Mesh Talk peer is different from having a direct network route to it.** A post office can introduce opted-in peers and carry recipient-encrypted events without joining their LANs.

## Terms and shared assumptions

| Term | Meaning here |
| --- | --- |
| LAN discovery | Mesh Talk's signed multicast discovery among devices on the same LAN. Routers and Tailscale do not forward that multicast as a Mesh Talk feature. |
| Routed discovery | A bounded, identity-free unicast query to a candidate IP, followed by a signed Mesh Talk response. On desktop, an already-running Tailscale client can supply candidate IPs on a best-effort basis. A user can also enter an endpoint. The headless CLI can use `/probe <ip:discovery-port>`. |
| Post office | A Mesh Talk node started with `--post-office` that stores and forwards opaque, recipient-encrypted events for offline delivery. This role alone does **not** introduce peers across LANs. |
| Bridge directory | An additional post-office feature enabled with `--bridge`. It introduces only devices that opt in, register their own signed public presence over a Noise-authenticated connection, and remain active. |
| Federation | Equal post offices running `--post-office --bridge` with the same trusted `--federation-peer` seed set. Each automatically discovers trusted relays on its LAN, probes routed seeds, exchanges opted-in signed presence, and reconciles opaque encrypted mailboxes over Noise. |
| Legacy gateway | Optional `--bridge-peer` compatibility path that proxies a client directly to one remote relay. It is not an IP router or a required federation hub. |

For the examples below, A and B are on LAN X, C and D are on LAN Y, E is a post office on LAN X, and F is a post office on LAN Y. All named clients run compatible Mesh Talk versions, are publicly visible, and have the needed application ports allowed. A discovery response proves a device's Mesh Talk key; it does not mean the user has manually verified its safety number or authorized every conversation.

| Topology | Tailscale members | Post offices | Bridge opt-in | Cross-LAN result |
| --- | --- | --- | --- | --- |
| 1. Only A ↔ C | A, C | None required | None | A and C discover and message each other directly; B and D remain local-only. |
| 2. One shared E | E, C | E with `--bridge` | A, B, C | A, B, C discover one another and exchange encrypted messages through E when no direct path exists; D remains local-only. |
| 3. E ↔ F | E, F | E and F with the same federation peer set | A, B, C, D | Relays exchange signed presence and encrypted mailboxes. Clients only need their reachable local relay; other routes are optional fallbacks. |
| 4. Three-site federation | D, G, H as available | All relays run the same role and trusted seed set | Opted-in clients on X/Y/Z | Each relay gossips through reachable peers; a disconnected site queues encrypted messages until a path returns. No hub is elected. |

### 1. A and C use Tailscale; B and D do not

```text
LAN X                         existing Tailscale path                         LAN Y
B ⇄ A  ──────────────────────────────────────────────────────────────────  C ⇄ D
    Mesh Talk direct Noise/TCP between A and C
```

Assuming LAN discovery works and the desktop Tailscale candidate adapter finds the other endpoint:

| Client | Mesh Talk peers it can discover automatically |
| --- | --- |
| A | B and C |
| B | A |
| C | A and D |
| D | C |

A and C can exchange messages directly over their Tailscale IPs. A does **not** learn D through C, and B does **not** learn C through A. Neither A nor C forwards LAN discovery, messages, or arbitrary traffic for the other LAN. No post office is needed for this live direct path; an ordinary post office may still provide its existing offline-delivery service where reachable. On the headless CLI, Tailscale inventory is not automatic; use an explicit probe or endpoint.

### 2. E on LAN X is the only bridge-enabled post office

```text
LAN X                                              LAN Y
A ─┐
   ├── E (`--post-office --bridge`) ══ Tailscale ══ C ── D
B ─┘      authenticated directory + opaque mailbox
```

Only E and C need a mutually reachable routed path. A and B reach E locally; C discovers E over Tailscale or an explicit endpoint. A, B, and C must each enable **Discover peers through a post office** (CLI: `--bridge-discovery`) and keep post-office relays enabled. E does not need `--bridge-peer` in this topology.

E accepts each client's own signed presence over an authenticated TCP session and returns the current participants' signed proofs. A/B see C; C sees A/B. Introduced clients are stored as *post-office routes*, not as fabricated direct client IPs. If A/B and C cannot connect directly, their messages are uploaded to E's opaque mailbox and drained by the recipient. Same-LAN A↔B and C↔D traffic can continue on direct routes.

D sees C on LAN Y but cannot automatically see A/B, and A/B cannot see D. C is **not** a subnet router or bridge-directory proxy. To include D, it must itself reach E and opt in, or LAN Y needs the F topology below. If E is unavailable, new cross-LAN introductions and delivery pause; existing roster entries may remain briefly, and pending outgoing messages remain queued locally for retry.

### 3. E and F federate two LANs

```text
LAN X: A, B ── E ═══ authenticated routed link ═══ F ── C, D :LAN Y
```

E and F use the same federation peer set and keep distinct keystores and local listener ports. Each client enables **Discover peers through a post office** and registers its own signed presence with a reachable local relay. E and F exchange those time-limited proofs, so A/B and C/D can discover one another without a client hub ID. The relays reconcile version-3 pairwise opaque mailboxes; both endpoints derive the same mailbox regardless of which relay holds it. New clients also write and read legacy relay-bound mailboxes for compatibility. Relays see no inner message contents, and the original 30-day mailbox arrival time follows a federated copy.

The directed relay link must carry signed UDP queries and Noise TCP sessions. LAN discovery finds trusted peers on the same LAN; `--federation-peer` supplies a routed bootstrap address and full identity pin for each relay. The seed list is the same on every relay. A legacy `--bridge-peer` gateway remains optional while old clients are in use.

### 4. Three or more sites with equal relays

```text
LAN X: clients ── C ── D ═══ routed path ═══ G ── clients :LAN Y
                       ╲                  ╱
                        ═══ H ── clients :LAN Z
```

Every relay uses the same federation role and trusted seed set. C and D discover each other locally; reachable routed links connect sites. Each relay periodically exchanges signed opt-in presence and changed encrypted mailboxes with every reachable trusted peer. There is no elected hub or required default relay. Clients may prefer one local post office, and their existing candidate order falls back to another reachable post office when that one is unavailable.

If D is the only path between X and Z, switching from D to C does not create a physical route to H. C retains encrypted outgoing mail until D or another routed path recovers. When a path returns, ordinary reconciliation deduplicates by signed event ID. Calls and direct device synchronization still require their own reachable paths.

The shared peer list is an **authorization boundary**: only listed Mesh Talk identities can obtain federation inventory. Each entry contains a full fingerprint and a numeric routed discovery endpoint, for example:

```sh
mesh-talk-node --keystore relay.keystore --password-file relay.pass \
  --name Relay --post-office --bridge --port 47475 \
  --federation-peer <C-id>@<C-tailnet-ip>:47474 \
  --federation-peer <D-id>@<D-tailnet-ip>:47479 \
  --federation-peer <D-id>@<D-lan-ip>:47479 \
  --federation-peer <G-id>@<G-tailnet-ip>:47474
```

Repeat the same `--federation-peer` entries on all relays; each skips its own entries. More than one signed endpoint may name the same relay identity. This lets each site use its reachable LAN or routed path when multicast is unavailable, including hosts that share a discovery port with a desktop app. Keystore, password file, name, and local ports are host-specific.

### Required reachability and setup

| Topology | Application ports that must be reachable | Relay flags |
| --- | --- | --- |
| A ↔ C direct | A and C: discovery UDP 47474 and each other's signed Mesh Talk TCP listening port over the routed path; LAN multicast for A/B and C/D. | None required. |
| E shared by A/B/C | A/B → E on LAN X and C → E over the routed path: discovery UDP 47474 and E's signed TCP listening port. | E: `--post-office --bridge --port 47475`; no `--bridge-peer`. |
| E ↔ F | Local clients reach their own relay; E and F can exchange signed UDP queries and Noise TCP over the routed path. | Both: `--post-office --bridge` and the same `--federation-peer` entries. |
| Three-site federation | At least one routed chain connects each site to the others; direct all-to-all paths improve resilience but are not required. | All relays use the same trusted seed set; clients keep bridge discovery opt-in. |

Port numbers are examples. Keep each relay keystore stable so its pinned identity survives restarts. Tailscale carries IP traffic but Mesh Talk neither installs nor configures it. A routed seed may fail; LAN-discovered trusted peers and other reachable seeds are still tried.

## Configuration and security boundaries

- **Client choice:** Bridge discovery is off by default and unavailable in invisible mode. Post-office use is a separate setting, on by default; bridge discovery requires it. Enabling bridge discovery shares this device's public signed presence with the relay's other current participants and permits automatic mailbox recovery for those introduced peers. Disabling it immediately clears local bridge introductions; remote registration expires without renewal after 45 seconds.
- **Relay choice:** `--post-office` alone provides store-and-forward service. Add `--bridge` for the opt-in directory. Provide the same `--federation-peer` identity and endpoint set on every federated relay. The older `--bridge-peer` gateway can remain during migration. E/F may be headless CLI nodes; they do not need the desktop Tailscale inventory adapter.
- **Multi-site preference and failover:** Clients use a local relay first by default; a full fingerprint is an optional mailbox preference. Each client can send to another reachable relay when its preferred one is unavailable. Federated mailboxes then converge through the relay graph; legacy relay-specific mailboxes remain available during migration. This preserves local-site offline delivery; cross-site discovery and delivery during an inter-site outage require an independent route and directory path between the remaining sites.
- **Identity:** A Tailscale IP, tailnet node name, directory entry, or gateway address is an address hint, not Mesh Talk identity or permission. Discovery proofs are signed, relays are pinned by full fingerprints, and connections use Noise identity binding. Manual safety-number verification remains a separate user action.
- **Transport:** Tailscale decides whether its overlay uses a direct link or DERP. Mesh Talk does not manage Tailscale, route IP packets between LANs, or provide a general TCP proxy to clients. WebRTC calls have a separate ICE media path; discovery and message success do not establish cross-network call support.
- **Linked devices:** Pairing and subsequent synchronization of available history, attachments, and settings require an authenticated connection between the two devices. The post-office bridge can introduce siblings and carry encrypted messages, but its gateway forwards only to the remote relay. If siblings have no direct reachable route, pairing and catch-up wait for one; E/F do not proxy those streams between clients.
- **Files and outage behavior:** The direct A↔C Tailscale test covers a consented file transfer. Older gateway tests cover discovery and encrypted DMs. Relay federation has separate targeted mailbox and presence tests; cross-site calls still need a media path. Existing post-office file delivery still requires the recipient-consent flow described in the [macOS relay test](post-office-macos-service.md#cross-machine-delivery-test). Federated encrypted mailboxes replicate among trusted relays; legacy mailboxes remain relay-specific.

## What has been verified

| Rig | Observed result | Boundary of that result |
| --- | --- | --- |
| `python3 scripts/e2e-tailnet-macos.py mac2015` | Signed routed discovery, two-way DMs, consented file transfer with matching SHA-256, and desktop automatic Tailscale inventory discovery. | Two Macs; WebRTC and forced DERP were not tested. |
| `python3 scripts/e2e-tailnet-bridge.py mac2015 --windows-host windows10` | A/current Mac and B/Windows exchange LAN messages; C/mac2015 exchanges messages with both through E; a queued C→B DM survives E's same-identity restart. | One bridge; D is not part of this rig. |
| `python3 scripts/e2e-tailnet-two-bridges.py mac2015 --windows-host windows10` | A/current Mac, B/Windows, C/D/mac2015 discover one another through E/F; all 12 directed encrypted DM paths pass. | Discovery scopes are isolated by test ports. The machines share one physical LAN, so this does not prove a two-site firewall, WAN, forced DERP, cross-gateway file transfer, or relay failover. |
| `python3 scripts/e2e-four-machine-tailnet.py` | On 2026-10-15, A/current Mac, B/Windows, E/mac2015, and F/C/D/mac-mini passed four-client discovery and all 12 directed encrypted DMs through E/F. Direct mac2015↔mac-mini and current Mac↔Windows Tailscale pairs passed signed discovery, two-way Noise DMs, and consented 4 KiB file transfers with matching SHA-256. The desktop runtime automatically discovered Windows through its Tailscale inventory. | Four physical machines, but C/D share mac-mini with F. Distinct discovery ports isolate scopes; this is not proof of separate LAN Y client hosts, forced DERP, WebRTC media, relay failover, or cross-relay file bytes. |
| `python3 scripts/e2e-four-machine-hub.py` | On 2026-10-15, C/D/X ran on mac2015, G on the current Mac, H/Z on mac-mini, and Y on Windows. The script chose D with a higher fingerprint than C, pinned all clients to D, verified gateway routes with dynamic proxy ports, and passed all six directed encrypted DMs among X/Y/Z. A Y→Z message stayed queued during D's outage and arrived after its same-identity restart. | Four physical machines and three isolated discovery scopes. G→D used LAN IP because the current Mac and mac2015 are in different Tailscale accounts; H→D used Tailscale. The scopes share physical LAN infrastructure, so this does not prove three independent LANs, forced DERP, file bytes, calls, or high availability. |
| `make e2e` | Backend process tests cover direct/routed DM, bridge introductions, offline relay delivery, channel and file CLI transfer, restart history, and a large-directory case. | Local process tests complement the cross-machine rigs. |
| `cargo test -p mesh-talk-core --test bridge_cross_lan three_sites_share_one_pinned_hub -- --ignored` | Seven local processes model three isolated discovery scopes: hub D and local C on X, gateways G/H on Y/Z, and one client per site. All clients discover the other sites and exchange encrypted DMs through D, using dynamically assigned gateway proxy ports. | Local loopback simulation; it does not prove a physical three-site Tailscale deployment or high availability. |

For protocol details and command examples, see [routed network discovery](routed-network-discovery.md). For what a relay can observe, see [post-office metadata privacy](post-office-metadata-privacy.md).
