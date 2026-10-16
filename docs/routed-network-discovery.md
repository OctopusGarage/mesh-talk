# Routed discovery and cross-network setup

Mesh Talk discovers compatible application endpoints across an existing permitted
IP route, including Tailscale. Mesh Talk never installs, configures or authorizes
Tailscale or its accounts/devices. It does not change VPN ACLs, merge LANs or trust
a device because it appears in a tailnet inventory.

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

For example, A/B share LAN X with relay E; C is on LAN Y and has an existing
permitted Tailscale connection to E. A/B need no Tailscale connection: their
physical LAN already provides direct reachability to E.

E runs `--post-office --bridge` with its persistent identity/password file and a
permitted TCP listener. A, B and C each explicitly enable **Use post offices** and
**Discover peers through a post office**, leave **Physical LAN discovery only**
off, and permit the required mailbox directions. Bridge discovery defaults to
off; enabling E alone does not opt its clients in.

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
