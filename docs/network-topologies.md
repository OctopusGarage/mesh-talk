# Network topologies: LAN, routed peers, and post offices

This guide maps the A–F examples discussed during routed discovery development to the behavior implemented on this branch. **Seeing a Mesh Talk peer is different from having a direct network route to it.** A post office can introduce opted-in peers and carry recipient-encrypted events without joining their LANs.

## Terms and shared assumptions

| Term | Meaning here |
| --- | --- |
| LAN discovery | Mesh Talk's signed multicast discovery among devices on the same LAN. Routers and Tailscale do not forward that multicast as a Mesh Talk feature. |
| Routed discovery | A bounded, identity-free unicast query to a candidate IP, followed by a signed Mesh Talk response. On desktop, an already-running Tailscale client can supply candidate IPs on a best-effort basis. A user can also enter an endpoint. The headless CLI can use `/probe <ip:discovery-port>`. |
| Post office | A Mesh Talk node started with `--post-office` that stores and forwards opaque, recipient-encrypted events for offline delivery. This role alone does **not** introduce peers across LANs. |
| Bridge directory | An additional post-office feature enabled with `--bridge`. It introduces only devices that opt in, register their own signed public presence over a Noise-authenticated connection, and remain active. |
| Gateway | A post office configured with `--bridge-peer` and the other relay's full Mesh Talk fingerprint. It exposes the remote relay's signed identity on a local TCP port and forwards encrypted TCP bytes to that relay. It is not an IP router. |

For the examples below, A and B are on LAN X, C and D are on LAN Y, E is a post office on LAN X, and F is a post office on LAN Y. All named clients run compatible Mesh Talk versions, are publicly visible, and have the needed application ports allowed. A discovery response proves a device's Mesh Talk key; it does not mean the user has manually verified its safety number or authorized every conversation.

| Topology | Tailscale members | Post offices | Bridge opt-in | Cross-LAN result |
| --- | --- | --- | --- | --- |
| 1. Only A ↔ C | A, C | None required | None | A and C discover and message each other directly; B and D remain local-only. |
| 2. One shared E | E, C | E with `--bridge` | A, B, C | A, B, C discover one another and exchange encrypted messages through E when no direct path exists; D remains local-only. |
| 3. E ↔ F | E, F | E and F with `--bridge` and pinned peers | A, B, C, D | After directory convergence, all four clients discover one another. Cross-LAN messages use one elected mailbox through the other relay's gateway when no direct route exists. |
| 4. Shared hub D | D, G, H | D is the bridge hub; G/H each pin D | Opted-in clients on X/Y/Z | Clients pin D's fingerprint. G/H expose D's signed identity through local gateway ports, so all three sites share D's directory and opaque mailbox. Another local post office C on X does not split cross-site delivery. |

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

### 3. E and F connect the two LANs

```text
LAN X                                                    LAN Y
A ─┐                                                   ┌─ C
   ├── E ═════ pinned Tailscale endpoint ═════ F ─────┤
B ─┘    `--post-office --bridge` on both relays          └─ D
       one elected opaque mailbox; other relay is a gateway
```

Only E and F need Tailscale. A/B reach E on LAN X; C/D reach F on LAN Y. Both relays run `--post-office --bridge`, and **each** has one configured `--bridge-peer <other-tailnet-IP>:<discovery-port>` plus the other relay's full 32-character Mesh Talk fingerprint in `--bridge-peer-id`. All four clients enable bridge discovery and post-office use. E and F must listen on different TCP ports, for example E on 47475 and F on 47476; the gateway binds the remote relay's signed TCP port locally. The relays' UDP discovery and TCP ports must be reachable over the existing routed path.

After both relay proofs and client registrations converge, A, B, C, and D each see the other three clients. The implementation elects the lowest-fingerprint post office from the verified roster. Clients on its LAN connect to it directly; clients on the other LAN connect to the elected relay's signed identity through their local gateway. The gateway forwards TCP bytes, while Noise authenticates the *remote relay*. The elected relay holds the opaque mailbox; the other relay does not copy its queued events. Direct A↔B and C↔D LAN routes remain preferred when available. The three-machine E2E test checks all 12 directed A/B/C/D message paths.

This is a two-relay topology with **one pinned remote relay per relay instance**. It is not an arbitrary multi-relay mesh, subnet router, mailbox replication scheme, or high-availability cluster. If the elected relay or the inter-relay path fails, new cross-LAN introductions and delivery pause; previously seen peers may remain in the roster briefly. Local direct LAN paths can still work.

### 4. Three or more sites with one explicit hub

```text
LAN X:  clients ── C (ordinary local post office)
                 └─ D (shared bridge hub) ⇐ Tailscale ⇒ G ── clients: LAN Y
                                        ⇐ Tailscale ⇒ H ── clients: LAN Z
```

D runs `--post-office --bridge` and has a stable keystore. G and H each run `--post-office --bridge` with **one** `--bridge-peer` endpoint pointing to D and `--bridge-peer-id` set to D's full fingerprint. They do not connect to each other. C can continue serving local devices, but each cross-site client pins D with `--bridge-hub-id <D-fingerprint>` (desktop: **Shared bridge hub fingerprint**) and enables bridge discovery. This makes D the sole directory and mailbox for those clients even if C has the lowest fingerprint on LAN X. A gateway client first registers with its local post office to learn D's signed proof and local TCP proxy port, then registers with D through that proxy. It refreshes the local gateway route while using D's directory.

Example relay flags, using site-specific discovery endpoints and persistent identities:

```sh
# LAN X: D; C may run separately with --post-office (and optionally --bridge).
mesh-talk-node --keystore d.keystore --password-file d.pass --name D --post-office --bridge --port 47475

# LAN Y: G; repeat on LAN Z for H with its own keystore and D's reachable endpoint.
mesh-talk-node --keystore g.keystore --password-file g.pass --name G --post-office --bridge \
  --bridge-peer <D-tailnet-IP>:47474 --bridge-peer-id <D-full-fingerprint> \
  --bridge-proxy-port 0

# Every participating CLI client; the desktop setting accepts the same fingerprint.
mesh-talk-node --keystore client.keystore --password-file client.pass --name Client \
  --bridge-discovery --bridge-hub-id <D-full-fingerprint>
```

Discover each site's local post office with LAN multicast or `/probe <site-relay-IP>:47474`. Clients on LAN X also need to discover D directly. `--bridge-proxy-port 0` asks the OS for a free local TCP port, so G and H need not match D's signed TCP port or each other's ports. The directory carries that route separately from D's signed identity, and Noise still authenticates D end to end. If D is unavailable, pinned clients keep outgoing work queued and do not silently switch cross-site mailboxes to C/G/H. Once D and the route return, delivery resumes. Each gateway has one pinned upstream; a larger deployment adds more site gateways that pin D, within the directory's 128-member limit.

This joins **opted-in Mesh Talk discovery and encrypted message delivery**, not arbitrary LAN multicast or IP traffic. D does not replicate mailboxes to C/G/H. Calls, linked-device catch-up, and cross-site file bytes still need their own reachable or consented paths.

### Required reachability and setup

| Topology | Application ports that must be reachable | Relay flags |
| --- | --- | --- |
| A ↔ C direct | A and C: discovery UDP 47474 and each other's signed Mesh Talk TCP listening port over the routed path; LAN multicast for A/B and C/D. | None required. |
| E shared by A/B/C | A/B → E on LAN X and C → E over the routed path: discovery UDP 47474 and E's signed TCP listening port. | E: `--post-office --bridge --port 47475`; no `--bridge-peer`. |
| E ↔ F | A/B → E and C/D → F locally; E ↔ F over the routed path: discovery UDP 47474 and **both** relay TCP ports. E must be able to bind F's TCP port locally, and F must be able to bind E's locally. | E/F: `--post-office --bridge`, distinct `--port` values, and reciprocal `--bridge-peer` plus `--bridge-peer-id`. |
| Shared hub D | Each site client → its local relay; G/H → D over the routed path: discovery UDP 47474 and D's signed TCP port. The local proxy port on G/H must be reachable by that site's clients. | D: `--post-office --bridge`; G/H: `--post-office --bridge --bridge-peer <D-address>:47474 --bridge-peer-id <D-fingerprint> --bridge-proxy-port 0`; clients: bridge discovery plus D's hub fingerprint. |

The port numbers are examples; use each relay's signed listening port if configured differently. To configure E/F, start each once to obtain its persistent Mesh Talk fingerprint, stop it, then restart it with the other relay's Tailscale IP, discovery port, and full fingerprint. Keep each relay's keystore and password file so its pinned identity does not change. The [routed discovery guide](routed-network-discovery.md#two-lans-with-one-post-office-on-each-lan) has the concrete E/F flag examples. Mesh Talk never installs, logs into, or modifies Tailscale.

## Configuration and security boundaries

- **Client choice:** Bridge discovery is off by default and unavailable in invisible mode. Post-office use is a separate setting, on by default; bridge discovery requires it. Enabling bridge discovery shares this device's public signed presence with the relay's other current participants and permits automatic mailbox recovery for those introduced peers. Disabling it immediately clears local bridge introductions; remote registration expires without renewal after 45 seconds.
- **Relay choice:** `--post-office` alone provides store-and-forward service. Add `--bridge` for the opt-in directory. Add a pinned `--bridge-peer` and `--bridge-peer-id` on *both* E and F only for the two-relay topology. E/F may be headless CLI nodes; they do not need the desktop Tailscale inventory adapter.
- **Multi-site hub:** Configure every participating client with D's full fingerprint. Each site's gateway pins D independently and may use `--bridge-proxy-port 0` for an assigned local port. The hub does not need a gateway peer. A configured hub is never replaced by a different post office for mailbox delivery.
- **Identity:** A Tailscale IP, tailnet node name, directory entry, or gateway address is an address hint, not Mesh Talk identity or permission. Discovery proofs are signed, relays are pinned by full fingerprints, and connections use Noise identity binding. Manual safety-number verification remains a separate user action.
- **Transport:** Tailscale decides whether its overlay uses a direct link or DERP. Mesh Talk does not manage Tailscale, route IP packets between LANs, or provide a general TCP proxy to clients. WebRTC calls have a separate ICE media path; discovery and message success do not establish cross-network call support.
- **Linked devices:** Pairing and subsequent synchronization of available history, attachments, and settings require an authenticated connection between the two devices. The post-office bridge can introduce siblings and carry encrypted messages, but its gateway forwards only to the remote relay. If siblings have no direct reachable route, pairing and catch-up wait for one; E/F do not proxy those streams between clients.
- **Files and outage behavior:** The direct A↔C Tailscale test covers a consented file transfer. The two-relay test covers discovery and encrypted DMs, **not** end-to-end file bytes or calls across both gateways. Existing post-office file delivery still requires the recipient-consent flow described in the [macOS relay test](post-office-macos-service.md#cross-machine-delivery-test). Mailboxes do not replicate between E and F.

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
