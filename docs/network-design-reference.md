# Mesh Talk network design reference

**Design review and implementation baseline · 2026-10-16.** This is the consolidated reference for deciding what Mesh Talk's network behavior **should** be. It includes implementation facts so that a desired behavior is not mistaken for an existing guarantee. The implementation tracker below records subsequent focused changes separately from unverified topology claims.

**Reading key:** **Current** describes the repository today; **Recommended** is the proposed product rule; **Open** needs a decision; **Validate** requires a targeted test before claiming support. The [topology guide](network-topologies.md) owns the current switch/port matrix and diagrams; the [scenario Q&A](network-design-qa.md) gives shorter answers. [Architecture](ARCHITECTURE.md) owns protocol detail, and [relay metadata privacy](post-office-metadata-privacy.md) owns the exact encryption boundary.

## 1. Purpose and boundary

Mesh Talk is a local-first, end-to-end encrypted conversation system. Devices should communicate directly when they have an authenticated application route. A peer-operated **post office** can temporarily hold recipient-encrypted events and exchange eligible information with other trusted post offices. Tailscale can supply IP reachability, but Mesh Talk neither creates a VPN nor merges LANs into one broadcast domain.

Here, **direct** means a Mesh Talk client contacts another named application endpoint without a Mesh Talk post office. It does not assert that the underlying Tailscale connection is physically peer-to-peer; Tailscale can use its own network relays. Routed discovery also needs a candidate endpoint: the desktop can obtain online peer IPs from the local Tailscale client, while manual/CLI probing supplies other known endpoints. A tailnet IP alone does not guarantee automatic discovery.

The intended result is **similar user experience across reachable sites**, not identical network behavior. Same-LAN multicast, routed unicast discovery, direct encrypted sync, directory introductions, relay custody, and relay federation are separate operations. The distinction determines what can be discovered, who can deliver a message, and what survives an outage.

### Glossary

| Term                                 | Meaning and important boundary                                                                                                                                                                                                                                             |
| ------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Client / Mesh Talk app**           | A device running the chat application. A client does not forward its LAN neighbors' discovery or act as a post office simply because it has Tailscale.                                                                                                                     |
| **Device identity / account**        | A device has signing and key-agreement keys and an account certificate. One account may own multiple devices; deduplicate devices by verified device identity, then group them under an account.                                                                           |
| **Peer**                             | Another verified Mesh Talk device, including a post office in the connection view. A post office is infrastructure and must not appear as an ordinary person or group member.                                                                                              |
| **Physical LAN**                     | A local interface/discovery scope in which application UDP multicast or unicast and TCP may be permitted. Wi-Fi client isolation, firewall rules, VLANs, and blocked ports can still prevent contact.                                                                      |
| **Direct application route**         | A permitted UDP discovery and/or TCP data path between two particular devices. It may use a physical LAN, routed IP, or Tailscale. A route is not identity proof.                                                                                                          |
| **Tailscale path**                   | Routed IP reachability between particular enrolled machines, subject to the existing tailnet policy and local firewall. It does not forward Mesh Talk multicast or automatically expose the machines' LAN neighbors.                                                       |
| **Discovery / presence**             | Signed, short-lived evidence that a device is available. Local discovery is multicast plus unicast fallback; routed discovery queries known candidate IPs; an opted-in relay directory can introduce remote devices. None grants conversation access.                      |
| **Directory introduction**           | A relay-provided, verified identity and potential relay route for an opted-in device. It is not a direct IP route, friendship, group membership, or proof that the recipient is online now.                                                                                |
| **Post office / relay**              | A peer-operated application mailbox service. It accepts and forwards opaque recipient-encrypted events and, with bridge mode, exchanges eligible opt-in presence with trusted relays. It is not an IP router, LAN broadcast repeater, group member, or WebRTC media relay. |
| **Host versus node**                 | One physical computer may run both the app and a separate post-office process. They have distinct Mesh Talk identities and may advertise different reachable interface addresses even though they share hardware.                                                          |
| **Federation**                       | Mutual, authenticated exchange between trusted post offices over an actual reachable link. Equal protocol roles do not mean identical device keys, ports, addresses, or identical live routes.                                                                             |
| **Event / sync**                     | A signed, content-addressed conversation record. Authorized peers reconcile event logs; group members may provide retained history to one another. Chat content is not sent in discovery announcements.                                                                    |
| **Custody / delivery / read**        | **Custody:** a relay has an encrypted copy. **Delivered:** the exact recipient produced a validated receipt. **Read:** a separate recipient action, if represented. A TCP session or relay upload alone establishes neither delivery nor reading.                          |
| **Direct recipient / known contact** | A verified identity that the sender already knows. A known contact can sometimes be addressed without appearing in a public cross-site directory, provided an authorized data path exists.                                                                                 |
| **Visibility**                       | Whether a device publishes public presence locally/directly or opts in to cross-site directory sharing. Invisible/private-contact behavior has additional authorization rules.                                                                                             |
| **Channel / group**                  | An authorized conversation with accepted members and encryption keys, not a network multicast group. A relay carrying a sealed group event never acquires membership.                                                                                                      |

### The three kinds of “broadcast”

| Phrase                            | Actual operation                                                                                                                             | Who receives it                                                                                                                       |
| --------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| **LAN announcement**              | A device sends its own signed presence over IPv4 multicast, with unicast reply/scan fallback.                                                | Devices in that permitted local UDP scope. Tailscale does not carry the multicast onward.                                             |
| **Cross-site directory exchange** | Trusted post offices synchronize fresh, self-signed presence from clients that opted in.                                                     | Enrolled relays and clients querying eligible directories. This is authenticated application sync, not a LAN broadcast of every user. |
| **Group message delivery**        | The sender creates encrypted, signed events for accepted members; direct fanout, member recovery and optional opaque relay copies move them. | Authorized recipients only. No group plaintext is sent over UDP multicast.                                                            |

## 2. Responsibility map

Each boundary should have one owner. The UI presents policy and status; it does not invent routes or decide that relay custody equals delivery.

| Owner                      | Repository area                                                           | Responsibility                                                                                                                                    | Must not do                                                                             |
| -------------------------- | ------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| Desktop UI                 | `frontend/src/`                                                           | Present people, groups, direct/relay status and settings; exclude relay-role devices from person/group pickers; explain pending versus delivered. | Infer a remote LAN from a Tailscale address or independently implement relay selection. |
| Desktop IPC/settings       | `src-tauri/src/`                                                          | Validate and persist user choices, pass them to the core, publish state changes.                                                                  | Duplicate crypto, discovery, or route-policy logic.                                     |
| Node orchestration         | `crates/mesh-talk-core/src/node/`                                         | Coordinate identity, roster, route candidates, direct delivery, relay upload/drain, retry and receipts.                                           | Treat a relay directory entry as direct TCP reachability.                               |
| Discovery                  | `crates/mesh-talk-core/src/discovery/`                                    | Produce and validate local/routed presence and maintain device/address observations.                                                              | Carry conversation content or authorize group membership.                               |
| Transport                  | `crates/mesh-talk-core/src/transport/`                                    | Establish identity-pinned Noise/TCP sessions over reachable IP endpoints.                                                                         | Trust IP or Tailscale identity as a Mesh Talk identity by itself.                       |
| Event log and crypto       | `crates/mesh-talk-core/src/{eventlog,identity,ratchet,channel}/`, `dm.rs` | Sign, encrypt, validate, order and reconcile conversation events; enforce DM/group keys and membership.                                           | Let a relay create membership or issue recipient delivery receipts.                     |
| File and media paths       | `crates/mesh-talk-core/src/file/` and call code                           | Enforce file offer/consent, encrypted chunk integrity, and separate call signaling/media rules.                                                   | Assume text-message success proves file completion or WebRTC reachability.              |
| Post office                | `crates/mesh-talk-core/src/postoffice/`                                   | Bound opaque mailbox storage, expiry, directory registration and trusted federation reconciliation.                                               | Decrypt inner content, publish all LAN users, or act as a general TCP/IP gateway.       |
| Local storage              | `crates/mesh-talk-core/src/storage/`                                      | Keep identities, accepted events and local history durable.                                                                                       | Claim post-office retention is the same as the user's local history retention.          |
| Existing network/Tailscale | Outside Mesh Talk                                                         | Provide permitted IP connectivity between endpoints.                                                                                              | Supply Mesh Talk authorization or automatically bridge physical LANs.                   |

**Recommended modular seam:** one core route-policy interface should order directly reachable, authenticated relays and apply preference/failover. Ordinary message uploads and consented file chunks now consume the same `post_office_candidates_for` ordering; directory refresh uses the shared bridge-eligibility gate. UI and any remaining feature paths should consume core policy instead of recreating candidate rules. The delivery-state model should likewise have one definition across backend and UI.

### Feature summary and routing ownership

| Feature                          | Required authority and payload                                                                                | Route choices and independent limit                                                                                                             |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| One-to-one messages and controls | Sender signs and encrypts events for the intended recipient; exact recipient receipts govern delivery status. | Direct Noise/TCP first; optional opaque mailbox custody and trusted relay federation for eligible paths. An online indicator is not a receipt.  |
| Group messages and history       | Accepted membership and sender keys govern event admission; sharing members can provide retained history.     | Direct fanout and member anti-entropy; optional per-recipient relay copies. One member's copy does not prove another member received the event. |
| Files and folders                | Recipient consent, encrypted chunks, manifests and integrity checks.                                          | Direct or eligible relay event paths; a file card arriving does not prove all bytes were saved. Chunk failover needs its own check.             |
| Calls                            | Signaling and WebRTC ICE media negotiation.                                                                   | Application routes may carry signaling; media needs its own reachable path. A post office is not a media relay.                                 |
| Linked devices                   | Account-certified devices and authenticated sibling synchronization.                                          | Ordinary event introductions may use reachable paths; full history/settings catch-up is a separate protocol responsibility.                     |
| Discovery and visibility         | Self-signed presence, user privacy policy and short expiry.                                                   | Local multicast/unicast, routed unicast, or opt-in trusted relay directory. Discovery carries no chat plaintext or membership grant.            |
| Offline custody                  | Recipient-encrypted, bounded mailbox copy with an original arrival time.                                      | Reachable post office and optionally trusted federation; 30-day maximum and capacity limits apply independently of local history.               |

## 3. Design philosophy and invariants

1. **Local first.** Attempt a verified direct route for a live recipient. A post office is optional for direct contact and valuable for offline custody or a cross-site destination without direct client reachability.
2. **Reachability is specific to an edge.** A↔D over Tailscale says nothing about B↔D, A↔E, or the two LANs as a whole. Every diagram edge requires its own permitted application path.
3. **Identity and authorization are separate from reachability.** Signed discovery and Noise authenticate a device; DM trust rules, private permissions and group membership decide what the device may receive or sync.
4. **Presence sharing is opt-in.** A post office advertises itself as infrastructure. It federates only fresh, eligible client presence. A relay's existence cannot opt all users on its LAN into cross-site visibility.
5. **Equal relays require trusted enrollment.** A relay may find another automatically, but discovery alone must not authorize directory/mail exchange. Enrollment uses verified identities; all enrolled relays perform the same protocol role. There is no global elected hub.
6. **Data stays opaque at relays.** Conversation bodies and applicable file data are encrypted for recipients. Relays still observe connection addresses, mailbox identifiers, sizes and timing; they can delay or drop traffic. Public discovery presence is signed, not confidential.
7. **Custody is bounded and best effort.** A relay copy has a **maximum** age of 30 days; byte/event limits can evict it earlier. Clients retain their own durable state and retry. No system can deliver across a permanent partition or after every available copy expires.
8. **Failure paths are explicit.** A preferred relay is a soft choice. Retry another directly reachable relay on failure; cross-site continuity requires that alternative to have a surviving path toward the destination. Deduplicate retransmissions by stable event identity.
9. **Conversation rules survive routing changes.** A relay cannot add a group member, grant a file offer, mark a recipient delivered, or provide a call media route. An accepted member may optionally share retained group history with other authorized members.
10. **Bounded work beats a fixed socket promise.** Limit directory pages, retries, concurrent dials, mailbox inventory exchange and group fanout. Measure at named scales; “one connection per relay” and “eight connections for 100 users” are not current guarantees.

### Recommended layered flow

```mermaid
flowchart TB
  UX[Conversation and user policy] --> EVENT[Signed events, membership and receipts]
  EVENT --> ROUTE[Direct route or optional encrypted relay custody]
  ROUTE --> AUTH[Signed discovery and identity-pinned Noise]
  AUTH --> IP[Permitted IP path: LAN, routed network or Tailscale]
  ROUTE --> FED[Trusted post-office federation]
```

**Open policy:** the current channel path can upload relay copies even after a direct attempt. Decide whether future custody is always attempted when enabled or only while recipient receipt remains unconfirmed. The latter saves relay work but changes offline recovery. Define this separately for each group recipient; a successful send to one member does not acknowledge another.

## 4. Settings and deployment configuration

### Client and channel switches

| Setting                                  | Current default and effect                                                                | If off or unavailable                                                                                                                  | Recommended interpretation                                                                                                                                                             |
| ---------------------------------------- | ----------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Use post offices**                     | On; client may upload and drain opaque mailboxes.                                         | Direct P2P still works. No mailbox upload/drain; the UI gates bridge discovery.                                                        | Keep send and receive together unless a concrete partial-mode use case is defined.                                                                                                     |
| **Discover peers through a post office** | Off; when on, a public device registers signed presence and accepts bridge introductions. | No new public cross-site directory introductions; direct LAN/routed discovery remains. Legacy known-peer mailbox paths may still work. | Cross-site public visibility needs per-device opt-in. It does not authorize a conversation.                                                                                            |
| **Preferred post office**                | Automatic; optional verified identity preference can reorder candidates.                  | Automatic selection favors locally discovered reachability.                                                                            | Show only directly reachable authenticated relays as normal choices; keep alternatives and return to preference when healthy. Legacy gateway routes need a distinct label if retained. |
| **Invisible mode**                       | Off; when on, suppresses public bridge presence and applies private-contact rules.        | Visible behavior follows other switches.                                                                                               | Do not equate invisibility with merely disabling federation.                                                                                                                           |
| **Channel: Share a copy**                | On for an accepted member.                                                                | Member receives its own events but does not serve retained channel history to others.                                                  | Only authorized members may be sync peers; disabling copies can reduce offline group recovery.                                                                                         |
| **Calls**                                | Off; enables call UI/signaling when on.                                                   | Text/file event paths continue.                                                                                                        | A call needs its own WebRTC ICE media route; a post office is not TURN.                                                                                                                |

These settings apply **per client device**, not to the whole LAN or account by implication. Two devices of one account may have different visibility or relay-use choices. A preferred relay does not pin a global hub for every participant.

**Effective client combinations** (assuming an otherwise reachable relay):

| Relay use | Bridge discovery     | Invisible | Effective behavior                                                                                                                                                                                           |
| --------- | -------------------- | --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Off       | Off                  | Off       | Public direct discovery and direct data only; no relay upload/drain.                                                                                                                                         |
| Off       | On in saved settings | Off       | **Implemented on this branch:** the core rejects directory refresh and clears relayed introductions when relay use is disabled. The saved bridge choice can take effect again after relay use is re-enabled. |
| On        | Off                  | Off       | Direct discovery plus opaque mailbox use for known peers; no new public cross-site directory introductions.                                                                                                  |
| On        | On                   | Off       | Direct paths remain; public presence is eligible for a reachable bridge relay's directory and trusted federation.                                                                                            |
| On        | On                   | On        | No public bridge registration; invisible/private-contact authorization governs any permitted communication.                                                                                                  |

For the fourth row, the relay also needs `--bridge` and a permitted client↔relay path. A user preference changes candidate order, not these visibility rules. A group member's **Share a copy** setting independently controls whether that member serves history to other accepted members.

The saved-flag interaction spans [settings application](../src-tauri/src/settings.rs) and [directory refresh](../crates/mesh-talk-core/src/node/bridge_client.rs). The core now checks relay use as well as bridge opt-in and invisibility, and clears earlier relay introductions when relay use is disabled. Existing proofs already copied to a remote relay still expire on their original short TTL; a disabled UI control alone is not a remote deletion request.

### Post-office modes

| Deployment                            | Configuration concept                                                                                 | Result and limit                                                                                                                                                    |
| ------------------------------------- | ----------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **P0: no relay**                      | Ordinary clients only.                                                                                | Direct discovery/sync and authorized member recovery; no post-office custody.                                                                                       |
| **P1: mailbox-only relay**            | `--post-office`, without `--bridge`.                                                                  | Holds opaque mail for known identities; does not introduce cross-site users or federate directories.                                                                |
| **P2: bridge relay**                  | `--post-office --bridge`; clients individually enable bridge discovery.                               | Maintains short-lived opt-in directory and v3 mailboxes for eligible clients. One bridge is a single-site dependency.                                               |
| **P3: several relays on one LAN**     | Each has its own persistent identity, host address and service port; equal role/trust policy.         | Clients can choose/fail over among directly reachable relays. Mailbox replication needs mutual federation trust; co-location alone does not replicate data.         |
| **P4: federated relays across sites** | P2 on each site, mutually pinned `--federation-peer` identities and reachable routed/Tailscale paths. | Synchronizes eligible signed presence and relay-independent encrypted mailboxes. Relays never gain direct routes to every remote client.                            |
| **P5: legacy gateway**                | `--bridge-peer` and pinned remote ID on a bridge relay.                                               | Compatibility forwarding to a remote relay for old clients. It is a proxy path, not equal-relay mailbox federation or an ordinary directly reachable client choice. |

**Same configuration** means same software version, role, trust policy, retention policy and protocol settings where appropriate. Every relay still requires unique persistent keys, identity, host-specific endpoints, credentials and a service that survives reboot. A trust set can be managed consistently without giving every host an identical address file. A relay without `--bridge` remains useful as a mailbox service but is not a cross-site directory member.

**Recommended enrollment UX:** discover candidate relays automatically on permitted links, show their verified identities, and enroll trusted federation peers explicitly before exchange. Current identity-pinned seeds provide the authorization boundary. **Open:** how to approve, rotate and revoke trust with minimal operator work. Revocation prevents future exchange but cannot retract previously copied metadata or ciphertext.

### Tailscale link shapes

| Link                                            | What the path enables                                                                                                                | What it does not enable                                                                         |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------- |
| Client ↔ client                                 | Signed routed unicast discovery and direct Noise/TCP sync if both application ports are permitted and a candidate endpoint is known. | Sharing either client's other LAN peers automatically.                                          |
| Client ↔ post office                            | That client can discover/authenticate the relay, register if opted in, and use its directory/mailbox if permitted.                   | The client's other LAN machines joining the relay's LAN or choosing it without their own route. |
| Post office ↔ post office                       | Candidate discovery and, after mutual trust, authenticated presence/mailbox federation.                                              | Automatic trust or a LAN-wide multicast bridge.                                                 |
| Two relays in separate tailnets/accounts        | Same as above **only if** policy and routing actually permit their UDP/TCP endpoints to communicate.                                 | Reachability or trust merely from knowing the other machine's name/IP.                          |
| One Tailscale machine with no Mesh Talk service | IP connectivity to that machine only.                                                                                                | Mesh Talk discovery, directory, storage or forwarding.                                          |

**Current discovery qualification:** desktop candidate collection from an installed Tailscale client is best effort; a manual address or CLI probe can be needed. A Tailscale IP remains just a candidate until a signed answer and identity-pinned Noise handshake succeed. Application UDP/TCP can be blocked even when a Tailscale machine itself appears online.

## 5. Concrete data flows

### A. Two directly reachable clients

1. A and B each announce **their own** signed presence on a permitted physical LAN, or A obtains B's routed candidate IP and sends a signed-response unicast discovery query.
2. A verifies B's identity and opens a Noise/TCP route. The direct sync layer exchanges authorized signed events; it does not send message text inside discovery packets.
3. A records outgoing work locally. B validates and stores a delivered event, then sends an authenticated receipt for tracked messages. Presence, a successful dial and an event merely held at a relay leave delivery unconfirmed.
4. If B is offline, A retries when a route returns. If relay use is on, a relay may hold a recipient-encrypted copy; the exact upload policy is the open decision above.

### B. One group with an offline member

1. A, B and C have accepted membership and the necessary keys. A sends a signed, encrypted group event; this is event fanout, not UDP broadcast.
2. B receives it directly; C is offline. If B has **Share a copy** on, C can later reconcile missing authorized events from B even if A is offline. A reachable post office may separately hold C's opaque copy when relay use is on.
3. C must pass the normal membership, signature and encryption checks. B and the relay cannot invite C, revive a removed member or invent a receipt for C. A direct receipt from B says nothing about C's delivery.

### C. Cross-site message through equal relays

```mermaid
flowchart LR
  subgraph X[LAN X]
    A[Client A] --> P[Local relay P]
  end
  subgraph Y[LAN Y]
    Q[Local relay Q] --> B[Client B]
  end
  P <-- "trusted Noise/TCP federation over a real route" --> Q
```

1. A and B each explicitly opt into cross-site public presence and register with a relay they can directly reach. P and Q already have a mutual trust relationship and an actual permitted link.
2. P/Q exchange B's eligible, fresh signed presence. A sees B as introduced **via relay**, not as a fabricated directly reachable LAN address.
3. If A lacks a direct route to B, A uploads B's recipient-encrypted event to P. P/Q reconcile the opaque mailbox; Q later serves B's drain. Q cannot read the inner content or mark it delivered.
4. B validates and stores the event, then produces a recipient receipt that can return along an available path. A may display **Delivered** only after validating that receipt. Directory expiry, relay custody expiry and local message history are different clocks.

### D. Multiple relays and route failure

```mermaid
flowchart LR
  A[Client A] --> P[Preferred local P]
  A -. "fallback when P fails" .-> R[Local R]
  P <-- "routed link" --> Q[Remote Q]
  R -. "only if a real alternate link exists" .-> Q
  Q --> B[Client B]
```

If P stops, A can try directly reachable R. **Case D1:** R↔Q survives; the encrypted event can reach B, subject to normal recipient drain and receipt. **Case D2:** R has no onward route; R may hold ciphertext, but B cannot receive until a path returns. **Case D3:** no local relay is reachable; A keeps its durable outgoing work and retries later. The UI must distinguish all three from delivered. **Recommended:** return to P when healthy while retaining R as an alternative; this automatic return requires validation. Copies on P and R must deduplicate at B by stable event identity.

## 6. Scenario catalog

Examples assume compatible software, verified application identities and allowed UDP/TCP unless the row names a restriction. **X: A/B/C**, **Y: D/E/F**, **Z: G/H/I** are ordinary clients. **P/Q/R/S** are post offices. A cell saying **Current** is described by existing guides/code; **Recommended** describes an intended rule; **Validate** needs independent-host evidence.

In the first diagram, the dotted edge exists **only** in scenarios T1–T3. Remove it for the fully isolated L7 case. The local lines represent potential direct paths, not guaranteed authorization or always-open connections.

```mermaid
flowchart LR
  subgraph X[LAN X]
    A[A] --- B[B] --- C[C]
  end
  subgraph Y[LAN Y]
    D[D] --- E[E] --- F[F]
  end
  subgraph Z[LAN Z]
    G[G] --- H[H] --- I[I]
  end
  A -. "optional direct Tailscale path" .-> D
```

A client-to-relay Tailscale edge is different: in R1, D can reach P, while E and F still cannot reach P merely because they share D's LAN.

```mermaid
flowchart LR
  subgraph X[LAN X]
    A[A] --- P[Post office P]
    B[B] --- P
  end
  subgraph Y[LAN Y]
    D[D] --- E[E] --- F[F]
  end
  D -. "Tailscale: directory and opaque mailbox" .-> P
```

### Local networks and direct routed clients

| ID  | Topology and settings                                          | Discovery / broadcast                                                                                                      | Data forwarding and outcome                                                                                                                                                                                                         |
| --- | -------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| L1  | X has A/B/C; no relay.                                         | Each public client announces itself within X.                                                                              | A→B is direct Noise/TCP; group events go to authorized members and recover from another sharing member. Offline B needs a later direct route. **Current.**                                                                          |
| L2  | L1 plus P, mailbox-only; A/B use relays.                       | Clients still find each other locally; P advertises only itself as a relay.                                                | A tries direct B and may upload an opaque copy to P. B drains later. P cannot introduce a remote user. **Current.**                                                                                                                 |
| L3  | L1 plus P, but A disables **Use post offices**.                | Local discovery is unchanged.                                                                                              | A cannot upload/drain through P. Direct A↔B and authorized group peer recovery continue. **Current.**                                                                                                                               |
| L4  | X has P/Q/R in bridge mode with mutual trust; clients opt in.  | Clients see local relays; eligible client presence may enter their authenticated directory sync.                           | Local direct delivery still applies. Multiple relays can hold/reconcile v3 opaque mail; preference/fallback is per reachable candidate. No relay appears in a channel member list. **Current with policy refinements recommended.** |
| L5  | X has P/Q/R but no mutual federation trust.                    | Each relay advertises itself, but no authorized relay-to-relay directory exchange.                                         | A may choose another relay on failure, yet a copy on P is not automatically replicated to Q/R. B must drain a relay that holds the copy. **Current boundary.**                                                                      |
| L6  | A/B share a physical network, but multicast is blocked.        | First-sight unicast and bounded local scan may help; firewall/client isolation can still block discovery.                  | If A learns B's endpoint and TCP is permitted, direct sync works. Same Wi-Fi name alone guarantees nothing. **Current boundary.**                                                                                                   |
| L7  | X, Y and Z are isolated; no client or relay has a routed link. | Each public client announces only within its own permitted LAN scope.                                                      | Direct/group sync stays within each site. No setting can manufacture a route between sites. **Physical limit.**                                                                                                                     |
| L8  | A client and P run as separate processes on one host.          | Each process signs and advertises its own identity; interface selection may yield different displayed IPs.                 | Their shared machine does not merge their roles. P stays out of the people/group-member list, and A still follows its own relay-use policy. **Current model; inspect addresses before diagnosing a bug.**                           |
| T1  | X and Y are isolated; A↔D only over Tailscale; no relays.      | A sees B/C locally and may find D by routed unicast. D sees E/F locally and may find A. No other cross-site introductions. | A↔D can directly chat or share an authorized group. B/C do not see D/E/F; E/F do not see A/B/C. Z remains isolated. **Current, subject to routed candidate/port reachability.**                                                     |
| T2  | T1 but A's routed UDP query or TCP port is blocked.            | A may fail to discover D, or may discover D but fail to sync; these are different failures.                                | No direct delivery until the required application path is restored or an independently reachable relay route is configured. **Current boundary.**                                                                                   |
| T3  | A↔D Tailscale works; A disables bridge discovery.              | Direct A↔D discovery can still work; A is absent from cross-site public relay directories.                                 | Known, authorized A↔D traffic stays direct. Bridge opt-out is not a physical-LAN-only firewall. **Current.**                                                                                                                        |

### One or more cross-site post offices

| ID  | Topology and settings                                                                     | Discovery / broadcast                                                                              | Data forwarding and outcome                                                                                                                                                                                                  |
| --- | ----------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R1  | P on X; D on Y has a direct Tailscale path to P; A/B on X and D opt in.                   | P can introduce the opted-in clients to one another. D does not export E/F merely by being on Y.   | A↔D uses P's opaque mailbox if no direct client route exists. P is a single cross-site dependency. **Current model.**                                                                                                        |
| R2  | R1, but D does not opt into bridge discovery.                                             | D is not a public cross-site listing; A/B cannot infer D from P's LAN.                             | A previously known D may still be reachable on an independently authorized direct or known-peer path. Public listing and delivery permission are distinct. **Current boundary / recommended clarity.**                       |
| R3  | P on X and Q on Y have mutual trust and a permitted Tailscale link; clients opt in.       | P/Q exchange only eligible signed presence over Noise, not LAN multicast.                          | A→D can use P→Q opaque mailbox federation; D drains Q and acknowledges. No direct A↔D IP path is created. **Current model.**                                                                                                 |
| R4  | P/Q are Tailscale-reachable but not mutually enrolled.                                    | They may discover each other's relay identity, but must not exchange client directory/mail data.   | They are independent mailboxes. Automatic trust-on-sight is **not recommended**. **Current boundary.**                                                                                                                       |
| R5  | P/Q are mutually trusted but Tailscale policy blocks the link.                            | No live federation despite matching config.                                                        | Mail and presence do not cross sites until a permitted route appears. **Current boundary.**                                                                                                                                  |
| R6  | X has P/R; Y has Q; P↔Q only. A prefers P, then P fails.                                  | A still sees R locally; any previously introduced remote presence may expire.                      | R can accept a copy, but cannot reach Q through failed P. Cross-site delivery waits. Adding R↔Q creates an alternate path. **Physical limit.**                                                                               |
| R7  | X has P/R; Y has Q; P↔Q and R↔Q. A prefers P, then P fails.                               | Directory sync may continue through R↔Q for opted-in clients.                                      | A chooses reachable R; R/Q move ciphertext, D drains and acknowledges. A should later resume P as preference. **Recommended; outage test required.**                                                                         |
| R8  | X has P/R, Y has Q, Z has S; only P↔Q↔S.                                                  | Eligible presence can propagate only across surviving trusted paths.                               | A→G may traverse P, Q and S as opaque mailbox reconciliation. Q failure partitions X from Z unless another trusted route exists. **Design model; multi-hop live proof required.**                                            |
| R9  | X has P/R, Y has Q, Z has S; fully connected relay links.                                 | Each trusted edge can reconcile eligible directory information; no network broadcast domain forms. | More paths improve availability but add pairwise inventory/handshake work; bound and measure at scale. **Design model, not a performance guarantee.**                                                                        |
| R10 | A on X can directly reach Q on Y via Tailscale, although P is also on X.                  | Q is a genuine directly reachable candidate for A; X's other clients do not gain that path.        | A may choose Q under user preference, but local-first automatic ordering should normally favor P. A remote relay visible only through P should not appear as an ordinary direct choice. **Current nuance / recommended UI.** |
| R11 | P on X and Q on Y run the same binary/flags but have different identities; no trust seed. | Each advertises itself in its own reachable scope.                                                 | Same executable does not create federation. Configure mutual trusted identities and a physical route. **Current boundary.**                                                                                                  |

### Policy, privacy, group and other feature combinations

| ID  | Topology and settings                                                         | Discovery / broadcast                                                                                                                                                          | Data forwarding and outcome                                                                                                                                                                                                                     |
| --- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| S1  | A is public locally, relay use on, bridge discovery off.                      | A announces directly but is absent from the public cross-site directory.                                                                                                       | Known-peer legacy mail may still use a relay; v3 federated participation is gated by opt-in. **Current.**                                                                                                                                       |
| S2  | A turns **Use post offices** off while P/Q remain federated.                  | Direct discovery is unchanged; UI disables the bridge switch. The core now clears prior relay introductions and gates directory participation even if bridge-on remains saved. | A stops ordinary mailbox upload/drain and public directory participation. **Focused regression test passes; remote registration expires on its existing TTL.**                                                                                  |
| S3  | A enables bridge discovery; B on the same LAN does not.                       | Trusted relays may introduce A across sites, not B.                                                                                                                            | Remote clients cannot infer B merely because it shares A's LAN or group. **Current privacy boundary.**                                                                                                                                          |
| S4  | A is invisible or a private contact.                                          | No public bridge registration; private authorization rules apply separately.                                                                                                   | A relay has no automatic exemption to reveal A. A known contact still needs the relevant authorized route. **Current boundary.**                                                                                                                |
| S5  | A sends to offline D through P/Q.                                             | A directory entry may expire while ciphertext remains in a mailbox.                                                                                                            | D drains when online, if a copy and route survive. Thirty days is an upper bound, not guaranteed storage. Relay custody does not mark Delivered. **Current.**                                                                                   |
| S6  | A/B/D share a group; D offline; B has **Share a copy** on.                    | Group membership is not advertised by UDP discovery.                                                                                                                           | D may recover authorized events from B or its own opaque mailbox. P/Q cannot become sync members. **Current model.**                                                                                                                            |
| S7  | Same group, but B disables **Share a copy** and A is offline.                 | Discovery may still show B online.                                                                                                                                             | B does not serve its retained group history; D needs another authorized sharing member or relay copy. **Current.**                                                                                                                              |
| S8  | A sends a file to D via federated P/Q.                                        | A file offer can arrive as an encrypted event.                                                                                                                                 | D must consent; manifest/chunk integrity and full byte completion need separate evidence. A card receipt is not file completion. File retry now tries the ordinary ordered relay candidates, but cross-site complete bytes remain **Validate**. |
| S9  | A calls D across sites.                                                       | Signaling may use available application paths.                                                                                                                                 | Media requires an independent WebRTC ICE route. P/Q are not TURN; encrypted text delivery does not prove a call works. **Current boundary.**                                                                                                    |
| S10 | A has two devices, one on X and one on Y.                                     | Each device has its own presence, settings and identity under one account.                                                                                                     | Roster groups the devices by account; delivery and recipient receipts remain device-specific where required. Full sibling history/settings sync needs its own authenticated path. **Current boundary.**                                         |
| S11 | Two trusted relays hold duplicate copies of one event.                        | Directory may contain multiple routes to one verified device.                                                                                                                  | Recipient validates and deduplicates by event identity; the UI shows one person/device, not one entry per relay. **Recommended invariant; test under failover.**                                                                                |
| S12 | Accepted group members span X, Y and Z; B↔C and D↔E use Tailscale; no relays. | Only directly connected member pairs discover each other; LAN rosters do not cross.                                                                                            | A→B→C→D→E→F can propagate by authorized group-log reconciliation while each bridge holds and shares its copy. This needs an end-to-end chain test. **Design model / Validate.**                                                                 |

### Illustrated walkthroughs for every catalog case

The diagrams below show an **example operation**, not a permanent socket or an IP route for every drawn pair. Solid arrows are application discovery/sync or direct data as labeled; dotted arrows are optional or failed operations. A missing arrow means that operation cannot be inferred from the topology. Each example assumes accepted conversation permissions where a chat/group event is shown. The quick tables above remain the index; these walkthroughs explain the sequence.

#### L1 — one LAN, no post office

```mermaid
sequenceDiagram
  participant A as A / LAN X
  participant B as B / LAN X
  participant C as C / LAN X
  A-->>B: Signed LAN presence; B verifies A
  A-->>C: Signed LAN presence; C verifies A
  A->>B: Direct Noise/TCP group event G1
  Note over C: C was offline during A's send
  C->>B: Later: reconcile accepted group log
  B-->>C: Missing event G1
```

**Result:** C receives G1 from B if B retained it and **Share a copy** is on. For a direct A→B message, B's authenticated receipt—not the LAN announcement—marks delivery. Without a later reachable holder, C continues waiting.

#### L2 — one mailbox-only relay on the LAN

```mermaid
sequenceDiagram
  participant A as A
  participant P as Relay P
  participant B as B
  A->>B: Attempt direct event M2
  Note over B: Offline
  A->>P: Upload B-encrypted opaque M2
  B->>P: Later: drain mailbox
  P-->>B: Copy M2
  B-->>A: Authenticated receipt
```

**Discovery:** A/B still learn one another locally; P announces only its own relay identity. P has no bridge directory, so it does not introduce a client from another LAN. P's acceptance is bounded custody, not B's receipt.

#### L3 — relay exists but sender disables relay use

```mermaid
sequenceDiagram
  participant A as A / relay use OFF
  participant P as Relay P
  participant B as B
  A-->>B: Signed local discovery
  A->>B: Direct Noise/TCP message M3
  Note over A,P: A does not upload or drain mailbox data
```

**Result:** B receives M3 directly when online. If B is offline, A waits and retries after a direct route returns; P's mere presence cannot override A's relay-use choice.

#### L4 — three trusted, equal relays on one LAN

```mermaid
sequenceDiagram
  participant A as A
  participant P as Preferred P
  participant Q as Relay Q
  participant R as Relay R
  participant B as B
  A->>B: Try direct event M4
  A->>P: Optional B-encrypted custody copy
  P-->>Q: Trusted v3 mailbox reconciliation
  Q-->>R: Trusted v3 mailbox reconciliation
  B->>Q: Later: drain reachable copy
```

**Discovery:** each relay advertises its own identity; opted-in client presence may be exchanged through their authenticated directories. The relays are equal in role but have separate keys/endpoints. B still receives one validated event even if several relay copies arrive.

#### L5 — three relays without mutual trust

```mermaid
sequenceDiagram
  participant A as A
  participant P as Relay P
  participant Q as Relay Q
  participant B as B
  A->>P: Upload B-encrypted M5
  Note over P,Q: No federation: P does not copy M5 to Q
  B->>Q: Drain Q: M5 absent
  B->>P: Drain P: M5 present
```

**Result:** Q can still be an alternate relay for a _new_ upload if A reaches it, but Q cannot serve the old P-held copy without federation. B must contact the holder or wait for A to retry elsewhere.

#### L6 — local multicast is blocked

```mermaid
sequenceDiagram
  participant A as A
  participant B as B
  A--xB: Multicast announce blocked
  A->>B: Bounded local unicast probe / manual candidate
  B-->>A: Fresh signed response, if UDP allowed
  A->>B: Identity-pinned Noise/TCP event, if TCP allowed
```

**Result:** unicast can recover discovery, but blocked application UDP or TCP still prevents the corresponding step. Two machines sharing a Wi-Fi name is insufficient evidence of reachability.

#### L7 — three completely isolated LANs

```mermaid
flowchart LR
  subgraph X[LAN X]
    A[A] --> B[B]
  end
  subgraph Y[LAN Y]
    D[D] --> E[E]
  end
  subgraph Z[LAN Z]
    G[G] --> H[H]
  end
```

**Operation:** A, D and G announce signed presence only within their own permitted local scopes. A→B, D→E and G→H can use direct encrypted sync. There is no X↔Y↔Z edge, so neither an opt-in switch nor a group membership record can deliver an event across the sites.

#### L8 — app and post office on one computer

```mermaid
sequenceDiagram
  participant A as App A on host H
  participant P as Relay P on host H
  participant B as B on same LAN
  A-->>B: Advertise A's app identity/address
  P-->>B: Advertise P's relay identity/address
  A->>B: Direct message M8
  A->>P: Optional B-encrypted custody copy
```

**Result:** B sees A as a person and P as infrastructure. A and P may show different interface IPs or ports; their distinct identities and roles matter more than the shared host. P is excluded from B's group-member picker.

#### T1 — one direct Tailscale client pair across LANs

```mermaid
flowchart LR
  subgraph X[LAN X]
    B[B] --- A[A]
    C[C] --- A
  end
  subgraph Y[LAN Y]
    D[D] --- E[E]
    D --- F[F]
  end
  A <-- "signed routed query; direct Noise/TCP event" --> D
```

**Operation:** A finds B/C locally and D through D's known Tailscale candidate. A sends D a direct encrypted message and D returns a receipt. B does not learn D/E/F through A; E does not learn A/B/C through D. A and D can sync an accepted group, but neither forwards all LAN discovery.

#### T2 — Tailscale machine reachable but application port blocked

```mermaid
sequenceDiagram
  participant A as A / LAN X
  participant D as D / LAN Y
  A--xD: UDP discovery query blocked
  Note over A,D: Or: signed query succeeds but Noise/TCP dial is blocked
  A->>D: Retry only after a valid route or relay path exists
```

**Result:** Tailscale status alone cannot place D in A's verified roster or deliver a message. A known identity does not manufacture a working application port; a separately reachable post office could provide a different route if configured.

#### T3 — direct Tailscale link with bridge discovery off

```mermaid
sequenceDiagram
  participant A as A / bridge OFF
  participant D as D / direct Tailscale peer
  participant P as Post office P
  A->>D: Routed signed discovery query
  D-->>A: Signed response
  A->>D: Direct Noise/TCP event
  Note over A,P: No A public directory registration
```

**Result:** A and D can communicate directly even though A is not publicly introduced by P. Turning bridge discovery off limits relay directory visibility; it is not a firewall against a directly reachable permitted peer.

#### R1 — one relay reachable from two LANs

```mermaid
sequenceDiagram
  participant A as A / LAN X
  participant P as P / LAN X
  participant D as D / LAN Y
  A->>P: Opt-in signed directory registration
  D->>P: Tailscale: opt-in signed registration
  P-->>A: Introduce D's verified identity/relay path
  A->>P: Upload D-encrypted event M1
  D->>P: Drain M1 over its own Tailscale path
```

**Result:** D's LAN neighbors E/F do not register merely because D can reach P. P cannot create direct A↔D TCP reachability; it is the mailbox path here. If P disappears, this single-relay cross-site route stops until P or another independently reachable route returns.

#### R2 — the remote client declines public bridge discovery

```mermaid
sequenceDiagram
  participant A as A / LAN X
  participant P as Bridge P
  participant D as D / bridge OFF
  A->>P: Query public directory
  P-->>A: No public D entry
  Note over A,D: A known, authorized D could use a separate valid route
```

**Result:** P does not advertise D's identity to A. A cannot infer D from D's LAN membership. A pre-existing contact/route is a separate question: opt-out of public listing does not itself delete an already authorized direct address or known-peer mailbox path.

#### R3 — two mutually trusted relays across LANs

```mermaid
sequenceDiagram
  participant A as A / LAN X
  participant P as P / LAN X
  participant Q as Q / LAN Y
  participant D as D / LAN Y
  D->>Q: Opt-in signed registration
  Q-->>P: Trusted Noise federation: D presence
  P-->>A: Introduce D via relay route
  A->>P: Upload D-encrypted M3
  P-->>Q: Reconcile opaque v3 mailbox copy
  D->>Q: Drain M3; validate and store
  D-->>A: Recipient receipt by an available route
```

**Result:** No LAN multicast or plaintext crosses P↔Q. A knows D as an introduced Mesh Talk identity, not as a fabricated LAN neighbor. A's delivered status requires D's validated receipt.

#### R4 — relays can see one another but are not trusted

```mermaid
sequenceDiagram
  participant P as P / LAN X
  participant Q as Q / LAN Y
  participant A as A / LAN X
  participant D as D / LAN Y
  P-->>Q: Discover relay candidate identity
  Note over P,Q: No mutual federation enrollment
  A->>P: Upload D-addressed event if D is already known
  Note over P,Q: No authorized directory/mailbox exchange
```

**Result:** A Tailscale link and matching relay software are insufficient. P and Q remain independent stores, so P's copy does not appear at Q. **Recommended:** automatic candidate discovery may simplify setup, but must not silently grant federation trust.

#### R5 — relays trust one another but the routed link is blocked

```mermaid
sequenceDiagram
  participant A as A
  participant P as P / LAN X
  participant Q as Q / LAN Y
  participant D as D
  A->>P: Upload D-encrypted M5
  P--xQ: Federation UDP/TCP blocked
  Note over P,D: M5 remains at P within retention/capacity limits
  P->>Q: Retry when permitted route returns
```

**Result:** Identity pins authorize exchange but do not provide IP reachability. Q cannot serve D's new mail or presence while the link is down; stale directory entries must expire rather than pretend D is freshly reachable.

#### R6 — preferred relay fails; fallback has no onward route

```mermaid
flowchart LR
  A[A] -. "preferred P failed" .-> P[P offline]
  A -->|upload M6| R[R reachable]
  Q[Remote Q] --> D[D]
```

**Result:** before the outage P had the only route to Q; after P stops, the diagram has **no R→Q edge**. A can hand encrypted M6 to R, but R cannot reach Q. D does not receive M6 now, and no recipient receipt exists. R holds it only within capacity/age limits; A must retain retry work. This is **local relay failover without cross-site continuity**.

#### R7 — preferred relay fails; fallback has its own remote route

```mermaid
sequenceDiagram
  participant A as A
  participant P as Preferred P / offline
  participant R as Alternate R
  participant Q as Remote Q
  participant D as D
  A--xP: Upload M7 fails
  A->>R: Upload same D-encrypted M7
  R-->>Q: Trusted federation over R-Q link
  D->>Q: Drain M7
  D-->>A: Validated receipt
```

**Result:** R is directly reachable from A **and** has a surviving R↔Q route. This is the topology that makes cross-site automatic fallback possible. Returning to preferred P and avoiding duplicate delivery after it restarts are **recommended behaviors needing an outage test**.

#### R8 — three-site relay chain

```mermaid
flowchart LR
  A[A / X] --> P[P / X]
  P <-- "trusted routed edge" --> Q[Q / Y]
  Q <-- "trusted routed edge" --> S[S / Z]
  S --> G[G / Z]
```

**Operation:** opted-in G registers with S; eligible presence must travel S→Q→P before A learns G through this chain. A's G-encrypted event travels P→Q→S as opaque mailbox reconciliation, and G drains S. If Q fails, X and Z lose their only relay chain. **Design model:** confirm multi-hop presence/mail convergence on real independent sites before promising it.

#### R9 — three-site fully connected relay graph

```mermaid
flowchart LR
  A[A / X] --> P[P / X]
  P <-- "trusted" --> Q[Q / Y]
  Q <-- "trusted" --> S[S / Z]
  P <-- "trusted alternate" --> S
  S --> G[G / Z]
```

**Operation:** when P↔Q fails, P can still exchange with S directly; when P↔S fails, P→Q→S may remain. Multiple paths can carry duplicate ciphertext, which G must deduplicate by event identity. More relay links improve route diversity but increase authentication and inventory work; a full mesh is **not** an unbounded scaling promise.

#### R10 — client can reach both a local and a remote relay directly

```mermaid
sequenceDiagram
  participant A as A / LAN X
  participant P as Local P
  participant Q as Q / LAN Y via Tailscale
  A-->>P: Signed local relay discovery
  A->>Q: Routed signed relay discovery
  Q-->>A: Verified Q identity and endpoint
  A->>P: Automatic local-first upload for M10
  Note over A,Q: Explicit healthy Q preference may reorder candidates
```

**Result:** Q can be a valid direct choice for A because A itself has a permitted Tailscale path to Q; B on A's LAN does not inherit it. A relay visible _only through P_ should be labeled as a gateway route, not offered as directly reachable.

#### R11 — equal binary and flags, but missing trust configuration

```mermaid
sequenceDiagram
  participant P as P / LAN X
  participant Q as Q / LAN Y
  P-->>Q: Signed candidate discovery, if a route exists
  Note over P,Q: Unique identities; no mutual federation pins
  P--xQ: Directory/mailbox exchange refused
```

**Result:** two `--post-office --bridge` deployments are not automatically one federation. Give each host its own persistent identity and reachable endpoint, then enroll trusted identities on both sides. Identical keys would be a deployment error, not equality.

#### S1 — relay custody on, public bridge discovery off

```mermaid
sequenceDiagram
  participant A as A / LAN X
  participant P as Post office P
  participant D as Known D
  A-->>P: Discover P as infrastructure
  Note over A,P: No A public bridge registration
  A->>P: Known-peer D-encrypted legacy mailbox copy, if eligible
  D->>P: Drain a known mailbox, if reachable
```

**Result:** A can remain absent from cross-site public introductions while still using eligible known-peer mailbox paths. D cannot discover A merely by asking P's public directory. Direct LAN or Tailscale discovery remains independent.

#### S2 — relay use off with a saved bridge-on setting

```mermaid
sequenceDiagram
  participant A as A / relay use OFF
  participant P as Relay P
  participant B as B
  A->>B: Direct encrypted event M2
  Note over A,P: Ordinary mailbox upload/drain disabled
  Note over A,P: Saved bridge-on flag remains, but core rejects directory refresh
```

**Result:** the core now blocks relay directory participation while relay use is off and clears prior introductions immediately, even when the saved bridge switch remains on. Existing remote registrations can remain visible until their bounded expiry; disabling one client cannot erase proofs already copied by a relay. The focused regression test covers local authorization and clearing, not a remote revocation round-trip.

#### S3 — only one of two LAN neighbors opts into cross-site presence

```mermaid
sequenceDiagram
  participant A as A / bridge ON
  participant B as B / bridge OFF
  participant P as P / LAN X
  participant Q as Q / LAN Y
  participant D as D / LAN Y
  A->>P: Register A's signed presence
  P-->>Q: Trusted exchange of eligible A proof
  D->>Q: Query directory; receive A, not B
```

**Result:** B may still chat locally with A, but B's location beside A cannot enroll B in the remote directory. If D is also a member of a group with B, group membership does not make B publicly discoverable.

#### S4 — invisible device and authorized private contact

```mermaid
sequenceDiagram
  participant A as Invisible A
  participant P as Relay P
  participant B as Authorized B
  Note over A,P: No public A bridge registration
  B->>A: Private/direct contact only if explicit policy and route allow
  A-->>B: Signed encrypted response if admitted
```

**Result:** P has no automatic exemption to publish A's presence. B's knowledge of A's identity is not enough by itself; the relevant private authorization and reachable data path are still required.

#### S5 — remote recipient is offline

```mermaid
sequenceDiagram
  participant A as A
  participant P as Local P
  participant Q as Remote Q
  participant D as D / offline
  A->>P: Upload D-encrypted M5
  P-->>Q: Copy opaque M5 with original arrival time
  Note over A,D: Status remains awaiting D; directory presence can expire
  D->>Q: Later: drain if copy survived retention/capacity
  D-->>A: Exact validated receipt
```

**Result:** D may receive M5 after coming online; the post-office maximum is 30 days and capacity can evict earlier. If every copy expires, A needs its own retained retry work and a new route. P/Q custody does not mean D read or received the message.

#### S6 — offline group member recovers from a sharing member

```mermaid
sequenceDiagram
  participant A as A / group member
  participant B as B / Share a copy ON
  participant D as D / offline member
  A->>B: Signed encrypted group event G6
  Note over A,D: A goes offline; D returns
  D->>B: Reconcile accepted group log
  B-->>D: Missing G6 and required ancestry
```

**Result:** D can recover without A or a post office because B has an authorized retained copy and a direct route to D. A relay copy, if separately enabled, would be another possible source; it never becomes a group member.

#### S7 — the only reachable group holder stops sharing

```mermaid
sequenceDiagram
  participant A as A / offline
  participant B as B / Share a copy OFF
  participant D as D / missing G7
  D->>B: Ask for missing group history
  B--xD: B does not serve its retained copy
  Note over A,D: Need another sharing member or surviving mailbox copy
```

**Result:** B may be online and may have read G7, yet D cannot recover from B under this setting. When A comes back or another authorized holder becomes reachable, D can try that source.

#### S8 — cross-site file offer and bytes

```mermaid
sequenceDiagram
  participant A as A
  participant P as Local P
  participant Q as Remote Q
  participant D as D
  A->>P: Encrypted file offer/manifest event
  P-->>Q: Opaque federation copy
  D->>Q: Drain offer; inspect and consent
  D->>Q: Signed exact-offer decision inside encrypted mailbox
  Q-->>P: Opaque consent copy
  A->>P: Drain and durably validate recipient consent
  A->>P: Encrypted chunk events after consent
  P-->>Q: Eligible chunks, if route/storage permit
  D->>D: Verify dense chunk chain and final hash
  D->>Q: Signed completion after durable verified save
  Q-->>P: Opaque completion copy
  A->>P: Drain exact recipient completion; finish delivery
```

**Result:** an offer or card reaching D is not proof that the file bytes arrived. Consent, size limits, exact chunk confirmation and integrity determine completion. Signed consent and completion now use the existing encrypted mailbox path, so neither client needs a direct listener. A two-relay loopback test covers sender restart, full bytes, exact recipient completion, stale decisions and a failed consent-store write alongside ordinary chat. A physical current-Mac→mac-mini test saves a 4096-byte attachment through the installed relays with matching SHA-256. File-chunk retry uses the shared recipient-aware candidate ordering. A forced relay outage during the complete physical file transfer remains a **validation gap**.

#### S9 — cross-site call request

```mermaid
sequenceDiagram
  participant A as A
  participant P as Post office P
  participant D as D
  A->>P: Application call signaling, if eligible
  P-->>D: Signal reaches D by an available path
  A-->>D: Separate WebRTC ICE media negotiation
  Note over A,D: Media requires its own working route
```

**Result:** P may help application signaling, but it is not a TURN or media relay. A text message or call invitation crossing sites says nothing about whether audio/video packets can flow.

#### S10 — two devices under one account

```mermaid
sequenceDiagram
  participant A1 as A1 / LAN X
  participant P as Local P
  participant Q as Remote Q
  participant A2 as A2 / LAN Y
  A1-->>P: Signed presence for device A1
  A2-->>Q: Signed presence for device A2
  P-->>Q: Eligible per-device introductions
  A1->>A2: Authenticated sibling sync, only with a valid path
```

**Result:** the roster may group A1/A2 under one account, but they retain distinct device identities, settings, routes and receipt targets. Ordinary relay visibility does not substitute for full sibling history/settings catch-up.

#### S11 — duplicate relay copies of one logical event

```mermaid
sequenceDiagram
  participant A as A
  participant P as Relay P
  participant Q as Relay Q
  participant D as D
  A->>P: Upload event ID E11
  A->>Q: Retry same event ID E11
  D->>P: Drain E11
  D->>Q: Drain E11 again
  Note over D: Validate and retain one logical event
```

**Result:** the UI should show one message and one verified device, not a row per mailbox or route. This deduplication invariant needs an explicit fallback/restart test; different outer relay envelopes must not cause duplicate conversation content.

#### S12 — group-member chain across three LANs, no post office

```mermaid
flowchart LR
  subgraph X[LAN X]
    A[A sends G12] --> B[B holds G12]
  end
  subgraph Y[LAN Y]
    C[C holds G12] --> D[D holds G12]
  end
  subgraph Z[LAN Z]
    E[E holds G12] --> F[F receives G12]
  end
  B <-- "direct Tailscale group sync" --> C
  D <-- "direct Tailscale group sync" --> E
```

**Operation:** all six have accepted group membership and the needed keys. A→B is a local signed-event sync; C later pulls G12 from B over their Tailscale edge; D gets it locally from C; E gets it from D over the second Tailscale edge; F gets it locally from E. Each intermediate member must retain and **Share a copy**, and each adjacent pair must discover/authenticate a real route. These intermediaries are actual group members who can read group content, unlike opaque post offices. A need not know every machine on Y/Z, and no LAN multicast or general IP routing crosses sites. If the sole bridge holder goes offline before the next hop copies G12, propagation waits or fails without another holder. **Design model; test the complete chain before promising eventual delivery.**

### Worked example: current Mac, Windows, mac2015 and mac-mini

Suppose the current Mac and Windows share LAN X with relays P (mac2015) and R (Windows). Q (mac-mini) sits on LAN Y and has a permitted Tailscale path to P. A separate client D runs on mac-mini's LAN. If Q and P are mutually trusted and D opts into bridge discovery, A on LAN X can learn D's signed presence through P/Q and send D a recipient-encrypted event through an available relay path. D's **own** route to Q is required; merely running Mesh Talk on the same machine as Q is not a substitute for testing a separate LAN Y client.

If P fails and R has no route to Q, choosing R cannot bridge the failure. Add and verify a trusted R↔Q path to achieve that failover. D may still communicate directly with a LAN Y peer without any relay. If A and D themselves have a permitted direct Tailscale route, they may communicate directly even when relay directory sharing is off. This is a conditional topology explanation, **not** a claim that all of these physical links and feature paths have been verified on the named machines.

## 7. Security, availability and scale checks

| Check               | Required interpretation or proposed acceptance evidence                                                                                                                                                                                                |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Relay trust         | Candidate discovery can be automatic; federation data exchange starts only after mutual verified enrollment. Include key rotation and revoked-relay behavior.                                                                                          |
| Metadata            | Message bodies remain opaque to relays, but IP/identity associations, mailbox IDs, sizes, counts and timing can be observed or correlated. A formerly trusted relay may retain what it already copied.                                                 |
| Message state       | Test local queue, relay-held copy, exact recipient receipt, duplicate/reordered copies and expiry separately. Do not conflate a green online dot with delivery.                                                                                        |
| Failure domains     | Stop a relay process, an entire host, a site LAN, and an inter-site route in separate tests. Fallback is meaningful only if an alternate end-to-end relay path survives.                                                                               |
| Group authority     | Test invite acceptance, removed members, key rotation, member recovery with **Share a copy** on/off and duplicate federation copies.                                                                                                                   |
| Files/calls/devices | Test consented file bytes and hash, not merely a file card; test WebRTC media separately; test linked-device catch-up separately.                                                                                                                      |
| Scaling             | Agree on client/relay/group sizes and measure concurrent dials, refresh traffic, mailbox inventory work, recovery latency and bytes per recipient. The current directory has a 128-member cap and the client queries at most eight relays per refresh. |
| Retention           | Test the 30-day maximum separately from byte/event eviction and local history. Surface expired/undeliverable work honestly.                                                                                                                            |

Federation should also converge after duplicate paths and partitions: use event identity for deduplication, expire stale presence, and bound inventory exchange, retries and per-peer work. These are scale and abuse-resistance requirements to measure rather than assumptions derived from the diagrams.

## 8. Current implementation deltas to review later

| Desired rule                                                                    | Present evidence or gap                                                                                                                                                                                                                                                                |
| ------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Disabling relay use also disables directory participation.                      | [Core directory refresh](../crates/mesh-talk-core/src/node/bridge_client.rs) now checks relay use, bridge opt-in and invisibility through one predicate; disabling clears introduced peers. A focused regression test passes. Remote expiry remains TTL-bound.                         |
| Relay preference and fallback apply consistently to all eligible data.          | Ordinary messages and consented file chunks use recipient-aware relay candidates. Exact-offer consent and completion travel in encrypted mailboxes. Physical cross-site bytes/hash and loopback sender-side completion are verified; a complete physical outage/recovery case remains. |
| Equal relays work at the intended scale.                                        | Current directory membership is capped at 128 per relay, clients query at most eight directories per refresh, and configured relays periodically reconcile inventories. No global one-socket-per-relay pool or large-federation performance target exists.                             |
| A preferred-relay outage still reaches an independent client on mac-mini's LAN. | Existing tests do not establish this entire physical topology with an alternate onward route, exact recipient receipt and independent client host.                                                                                                                                     |

These are implementation comparison points, not changes authorized by this design document.

## 9. Decisions for the design review

### Implementation and evidence tracker

| Requirement                                                          | Status on this branch                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Acceptance evidence still needed                                                                                                                                           |
| -------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Relay use off stops public directory participation                   | Implemented: one core eligibility check gates registration, in-flight result publication and bridge-derived recovery; disabling clears local introductions.                                                                                                                                                                                                                                                                                                                              | Confirm remote registrations disappear after their existing TTL on an independent relay.                                                                                   |
| Preferred relay failure falls back for consented file chunks         | Implemented: file retry walks the same recipient-aware ordered candidates as ordinary uploads; a focused test stops the preferred relay and verifies custody at another.                                                                                                                                                                                                                                                                                                                 | Complete file bytes, final hash, recipient confirmation and sender recovery in a federated two-site outage.                                                                |
| Consent and recipient completion work without a direct client route  | Implemented: signed, exact-offer revision controls use the existing encrypted pairwise mailbox and a separate durable protocol store. Loopback verifies full bytes, sender restart, completion return, invalid bindings, stale decision rejection and chat continuing after a consent-store write failure. A physical current-Mac→mac-mini transfer through installed relays saved matching SHA-256 bytes.                                                                               | Repeat full recipient completion and sender status under a forced physical relay outage; larger files/folders and independent client host coverage remain separate checks. |
| A selected relay remains the first attempt for introduced recipients | Implemented: one core candidate ordering keeps an explicit preference ahead of an introducing relay, then tries the introducing relay before unrelated fallback relays.                                                                                                                                                                                                                                                                                                                  | Custody at the preferred relay is not proof of an onward federation route or recipient delivery; a disconnected federation edge can still delay delivery.                  |
| Alternate relay preserves cross-site delivery                        | A loopback test now stops the preferred relay, moves recipient-encrypted mail through the local alternate and remote relay, and verifies the remote client decrypted it. R6 still demonstrates the missing-onward-path case.                                                                                                                                                                                                                                                             | Repeat on independent hosts and obtain a validated receipt back at the sender through the surviving route.                                                                 |
| Multi-hop federation and group-member chains                         | A targeted three-relay loopback test moves opaque mail P→Q→S and eligible presence S→Q→P. A four-host physical test completes all 12 directed encrypted DMs with two mutually pinned relays; cross-site client pairs use federation, while same-site pairs may connect directly. Clients see only their local relay. A four-member channel on the installed federation also delivers an event from each site to all other members. S12's full group-member chain remains a design model. | Independent three-site end-to-end tests, including a partition, late join and duplicate-path convergence; separately test S12.                                             |
| Visibility, retention and scaling                                    | Current bounds are 45-second directory presence, 128 members per relay, eight queried relays per refresh, and 30-day maximum mail age subject to event/byte eviction.                                                                                                                                                                                                                                                                                                                    | Product limits, backpressure and performance measurements at agreed client, relay and group sizes.                                                                         |
| Calls and linked devices                                             | Separate feature paths; text-message success does not establish them.                                                                                                                                                                                                                                                                                                                                                                                                                    | Feature-specific end-to-end evidence for media and sibling catch-up.                                                                                                       |

**Four-host checks (2026-10-16):** Disposable clients on the current Mac, Windows10 and mac-mini, with relays on mac2015 and mac-mini, discovered all expected clients and completed all 12 directed encrypted DMs in legacy-gateway, mutually pinned disposable-federation, and installed-federation modes. Cross-site client pairs used the relay path; same-site pairs could communicate directly. A four-member channel using the installed relays delivered A's and C's messages to each of the other three members. Federation clients discovered only their own directly reachable relay. A consented current-Mac→mac-mini attachment crossed both installed relays and saved with matching SHA-256. Direct mac2015↔mac-mini and current-Mac↔Windows Tailscale messages and consented files also matched SHA-256. Installed mac2015 and Windows relays each separately held and delivered an offline DM and private-channel event to a restarted recipient while the sender was stopped. These checks do not prove a three-site group-member chain, forced relay outage recovery, calls or linked-device catch-up.

**Evidence labels:** a focused unit/integration test proves its stated local behavior; it does not prove an independent host, a physical link, or a full feature path. Keep a requirement marked conditional until the corresponding topology and recipient-side outcome have been observed.

1. **Trust enrollment:** retain explicit identity pins as the authority boundary and design a simpler mutual enrollment workflow? **Recommended: yes.** Candidate detection may be automatic; trust should not be.
2. **Relay custody policy:** upload after every direct attempt, or only while a recipient receipt is outstanding? **Open.** Define cost, offline resilience and per-member group behavior before implementation.
3. **Known but unlisted contacts:** which authenticated paths may reach a contact that disabled cross-site public presence? **Open.** Public listing must remain distinct from private permission and actual route.
4. **Preference and return:** should a user-selected relay take precedence over a reachable local relay, and when should the client return after fallback? **Recommended:** explicit preference wins when healthy, automatic mode favors local, failed operations retry another candidate, then recover the preference.
5. **Feature promise:** which features must have cross-site relay failover in the first acceptance milestone? **Recommended:** explicitly scope text and group controls first; require separate criteria for full file bytes, calls and linked-device sync.
6. **Scale target:** what client/relay/group sizes and latency/traffic budgets are required? **Open.** Do not turn a topology sketch into an unsupported fixed connection ceiling.

Use these decisions to revise the design baseline before comparing it against code. The current [topology guide](network-topologies.md) and [scenario Q&A](network-design-qa.md) remain evidence and focused explanations; neither should silently convert a proposed rule into a shipped guarantee.
