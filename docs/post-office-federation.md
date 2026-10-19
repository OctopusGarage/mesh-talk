# Equal post-office federation

A federation connects equal post offices over reachable LAN or Tailscale paths.
Each relay keeps its own private identity and encrypted storage. All relays may
use the same **public trust manifest**; there is no primary hub. Discovery finds
candidates, but only explicitly pinned identities can exchange federation data.
Clients select a directly reachable relay and opt into bridge discovery separately.

## Responsibilities and boundaries

| Component      | Responsibility                                                                                                               |
| -------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| Client         | Encrypt/decrypt content, authorize peers and group membership, order directly reachable relays, validate recipient receipts. |
| Post office    | Temporarily retain opaque mailbox events and exchange copies and eligible signed presence with trusted relays.               |
| Trust manifest | Name approved relay identities and discovery endpoints; it contains no passwords or private keys.                            |
| LAN/Tailscale  | Supply actual permitted IP routes. Federation does not create a route that the network cannot carry.                         |

Message bodies, file consent, and completion controls remain encrypted for their
recipients. Relays can observe transport and mailbox metadata. Retention is at
most 30 days and may be shorter under storage limits. Custody is not delivery.

## One shared public configuration

Building this CLI requires Rust 1.89 or newer, including standard-library file
locking support for concurrent manifest updates.

Create a manifest using the current `mesh-talk-node` binary, without opening a
keystore or providing a password:

```bash
mesh-talk-node federation --file relays.conf init
mesh-talk-node federation --file relays.conf enroll d2c86ed557d201fa877f0ef72dc00a67@192.168.3.16:47478
mesh-talk-node federation --file relays.conf enroll edbbf0daac94d930e687ba86c20d3f06@192.168.3.72:47479
mesh-talk-node federation --file relays.conf enroll edbbf0daac94d930e687ba86c20d3f06@100.82.240.19:47479
mesh-talk-node federation --file relays.conf enroll e2bc2167aab97af10cb893de4ae2fb0c@100.109.255.49:47480
mesh-talk-node federation --file relays.conf list
```

These addresses describe the repository's example deployment; use the verified
identities and reachable endpoints of your own machines. Confirm fingerprints
through a trusted channel before enrollment. Multiple endpoints may name the
same relay, so each site can use a reachable LAN or Tailscale route.

The format is UTF-8, bounded to 64 KiB and 256 entries before deduplication:

```text
mesh-talk-federation-v1
# Full device fingerprint followed by numeric IP and discovery UDP port.
edbbf0daac94d930e687ba86c20d3f06@192.168.3.72:47479
edbbf0daac94d930e687ba86c20d3f06@100.82.240.19:47479
```

Deploy the same public file to every participating relay. Each ignores its own
identity when probing. Keep private keystores, passwords, names and listener
ports local to each host. File configuration is authoritative: it cannot be
combined with `--federation-peer` flags. Existing repeated flags remain supported
when no manifest is supplied.

For a manually started relay, add `--post-office --bridge --federation-config
/absolute/path/relays.conf` to its existing identity/listener arguments. The
manifest is validated before password loading or network startup. A header-only
manifest means **no trusted federation peers**.

## Boot services

The [macOS installer](post-office-macos-service.md) accepts
`--federation-config PATH`; the [Windows installer](post-office-windows-service.md)
accepts `-FederationConfig PATH`. Both validate the file with the supplied current
binary before installation changes, copy it into persistent service data as
`federation.conf`, and pass its absolute path to the boot service. This permits
removing the original staging directory after installation.

- macOS installed manifest: `~/.local/share/mesh-talk-post-office/federation.conf`.
- Windows installed manifest: `D:\ProgramData\MeshTalk\PostOffice\data\federation.conf`.

Continue using the installers' existing activation/restart process. Installing a
public trust file does not replace the service's private identity or password.
Legacy gateway arguments can coexist during migration; they are not required
for an equal federation with reachable relay paths.

## Enrollment, revocation and rotation

`enroll` adds an explicitly approved identity/endpoint. `revoke` removes **every
endpoint** for that identity:

```bash
mesh-talk-node federation --file /absolute/path/federation.conf revoke edbbf0daac94d930e687ba86c20d3f06
```

Distribute the updated file to every affected relay and restart those services.
Edit the installed persistent file, or rerun preparation with the updated source
file. **Running services retain their startup trust snapshot**; editing a file
alone does not revoke an active session. There is no hot-reload claim. `init`
refuses to overwrite an existing file. Successful mutations use a persistent
companion lock across load, atomic replacement and directory synchronization, so
concurrent enrollment/revocation cannot lose updates or restore revoked pins.
A contending mutation fails promptly with retry guidance. Mutations print the
restart requirement; read-only listing and startup read an atomic snapshot.

For key rotation, verify and enroll the replacement identity and its endpoints,
then remove the old identity on each affected relay and restart. Revocation
cannot reclaim ciphertext or metadata a formerly trusted relay already copied.

## Capacity and compatibility

Modern clients explicitly request directory v3 and admit up to 1,024 entries.
Federation inventory v2 carries the matching bounded presence snapshot. Client
v1/v2 directory requests and federation v1 keep their original 128-entry limits;
modern peers retry the older protocol on a fresh authenticated connection.
Mixed-version deployments therefore retain the smaller legacy visibility limit.

A client refresh visits all eligible directory relays with at most eight queries
in flight, and overlapping refreshes on that client are suppressed. Healthy
results are persisted/published as they complete rather than waiting for stalled
relays. Receipt timestamps, policy generations and rollback prevent canceled,
expired or obsolete results from being published. The ninth relay is no longer
permanently skipped. Directory concurrency is a separate scheduling bound. A client Node also shares
eight managed TCP slots across incoming/outgoing handshakes, active sessions and
retained idle sync sessions (at most seven per direction). Sequential sync may
reuse an exact-identity/route/policy session for up to 15 idle seconds; one-shot
protocols dial fresh. This is not one busy socket per relay. OS backlog, WebRTC
media and other processes are outside the Node budget.
Presence remains short-lived (about 45 seconds); forwarding does not renew its
origin expiry. These bounds are resource limits, not a measured throughput SLA.

The relay runtime separately limits seed discovery to eight concurrent probes
and reconciliation to eight concurrent relay sessions. Known trusted LAN routes
are scheduled before routed seed discovery, and successful seed responses are
consumed as they arrive. Routes are deduplicated by endpoint and both public
keys. These relay-runtime budgets are independent of the client Node TCP budget;
large graphs of slow or unreachable peers can still lengthen a reconciliation
round.

## Failure behavior and verification

Clients retain direct delivery and order relay alternatives. A failed relay can
be bypassed only if an alternate onward path actually reaches the destination.
A Windows relay cannot route around a failed Mac when it has no independent
path to the remote site. Reconciliation retries after routes return; signed IDs
provide deduplication and recipient receipts establish delivery.

Focused tests cover modern/legacy directory limits, more than eight eligible
relays, 130-member federation presence, legacy fallback, explicit trust rejection,
three-relay forwarding, and encrypted file consent/completion. The network
[reference](network-design-reference.md#implementation-and-evidence-tracker)
records physical evidence and remaining acceptance gaps separately.
