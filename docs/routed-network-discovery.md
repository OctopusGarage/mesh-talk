# Routed discovery and cross-network setup

Mesh Talk discovers compatible application endpoints across an existing permitted
IP route, including Tailscale. It does not install/configure a VPN, merge LANs or
trust a device because it appears in a tailnet inventory.

For communication principles, module contracts and illustrated cases, use the
[network architecture baseline](network-design-reference.md). This guide owns
candidate discovery and setup; [federation operations](post-office-federation.md)
own equal-relay enrollment/deployment.

## Boundaries

The desktop's optional Tailscale adapter reads an already available
`tailscale status --json`, checks running/online peers and omits self. Its bounded
snapshot provides up to 256 candidate addresses through the core `CandidateSource`
interface. Missing CLI/client permissions or incompatible output yield no
candidates; this is best effort, not proof of application capability.

A candidate receives a rate-limited UDP query. Only a fresh valid signed response
establishes Mesh Talk identity and a possible TCP listener. Actual data transport
rechecks pinned identity and admission through Noise. Discovery send success,
tailnet presence and TCP connectivity are different observations.

## Protocol and routes

The routed query (`MTDQ`, version 1) is a fixed 64-byte identity-free UDP packet
with a random 16-byte nonce. A public responder signs the `MTDR` response, binding
that nonce to its signed announcement. The requester admits one response from an
outstanding candidate IP within five seconds after validating both signatures.
Invisible mode and physical-LAN-only disclosure suppress routed probing/replies.

Roster routes retain origin, freshness and authentication observations separately
from device identity. A known direct route may be LAN, routed-query or
authenticated; a gateway/mailbox hint alone is not direct reachability. Transport
tries bounded candidate routes and keeps current signed ports ahead of stale
cached ports. IPv4/IPv6 support follows available sockets and interface scope.

## Connect two apps directly

1. Establish the actual permitted IP route outside Mesh Talk.
2. Run compatible Mesh Talk nodes and allow the target discovery UDP port plus
   its advertised TCP listener through the relevant ACL/firewall.
3. With the desktop adapter available, use **Connection → Rescan**. If automatic
   discovery is unavailable, enter the target endpoint in connection diagnostics.
   The CLI uses `/probe <ip:discovery-port>`.
4. Validate the discovered identity; normal private-contact/group permissions
   still apply. A blocked TCP listener can prevent delivery after discovery.

The default discovery port is 47474. A custom relay/app discovery port must be
queried explicitly or otherwise supplied as a candidate; Tailscale status does
not advertise Mesh Talk service ports. A device-to-device Tailscale edge exposes
those endpoints, not all machines on their neighboring LANs.

## Opt-in post-office bridge

A reachable relay E runs `--post-office --bridge` with its normal persistent
keystore/password source and a permitted TCP listener. Each public client enables
**Use post offices** and **Discover peers through a post office**, leaves
**Physical LAN discovery only** off, and permits the desired mailbox directions.

A client on another LAN can reach E directly through Tailscale and join E's
Mesh Talk discovery/mailbox network. E introduces eligible registered clients;
when the clients lack a direct route to one another, encrypted messages, consent,
file chunks and return controls use opaque mailboxes. Directory entries do not
substitute E's address for the recipient's own TCP listener or grant membership.

The preferred-relay picker includes known direct LAN/Tailscale candidates. A
saved unavailable preference is preserved; alternatives need a surviving onward
route to provide cross-site continuity. Calls still need live direct signaling
and separate WebRTC media. Full sibling replay has its own authenticated path.

## Two LANs with one post office on each LAN

E and F run the same bridge role with unique persistent identities and a shared
public `--federation-config` manifest (or matching inline `--federation-peer`
enrollment). They reconcile eligible presence and v3 opaque mailboxes only after
mutual trust and actual network reachability. A/B reach E locally; C/D reach F
locally; only E↔F needs the routed edge.

The older `--bridge-peer` gateway remains a compatibility path. It forwards a
pinned remote relay's Noise connection and does not itself replicate stores.
Legacy gateway test results must not be presented as equal-federation evidence.

## Two-Mac tailnet test and other focused checks

The opt-in `scripts/e2e-tailnet-macos.py <ssh-host>` rig checks direct signed
routed discovery, bidirectional encrypted DMs and consented file SHA-256 over an
existing two-Mac tailnet. Its header owns build prerequisites and cleanup.
`scripts/e2e-four-machine-tailnet.py` separately supports federation modes;
`scripts/e2e-federation-recovery.py` tests an actual surviving alternate chain and
partition recovery. [Script entry points](../scripts/README.md) distinguishes
these scenarios and the older gateway rigs.

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

Historical source research remains in [routed discovery research](routed-network-discovery-research.md).
External references: [Tailscale CLI](https://tailscale.com/docs/reference/tailscale-cli),
[status representation](https://github.com/tailscale/tailscale/blob/main/ipn/ipnstate/ipnstate.go),
[connection types](https://tailscale.com/docs/reference/connection-types).
