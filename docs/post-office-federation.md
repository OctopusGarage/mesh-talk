# Equal post-office federation

A federation connects equal post offices over reachable LAN or Tailscale paths.
Managed post offices all use UDP 47478 for their dedicated discovery endpoint,
plus UDP 47474 for shared LAN discovery, with main TCP 47479 for encrypted data. When migrating older per-host ports,
update every affected federation seed and legacy bridge endpoint together with
the service listener; preserve device fingerprints, credentials and custody.
Each relay keeps its own private identity and encrypted storage. All relays may
use the same **public trust manifest**; there is no primary hub. Discovery finds
candidates; explicit identity pins or operator-approved network memberships authorize
federation data exchange.
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

## Automatic membership in an approved network

Network mode removes pairwise federation configuration. An operator approves one
network authority; each relay holds its own expiring, nondelegating membership.
LAN discovery automatically finds relay candidates. Both sides verify membership
against their locally approved policy and authenticated Noise device identity
before sending mailbox inventories or opted-in presence. Discovery, co-location,
VPN access and a reported client version never grant membership.

Create the network on an enrollment workstation, keeping the private authority
key off every relay:

```bash
mesh-talk-node network init --authority-key authority.key --policy network.policy
```

Obtain each relay's full device fingerprint from its existing startup output and
verify it through a trusted channel. A newly deployed relay can first run without
network mode to establish its persistent identity. Issue its public approval:

```bash
mesh-talk-node network enroll --authority-key authority.key --policy network.policy \
  --device aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --membership alice-relay.membership --valid-days 30
mesh-talk-node network inspect --policy network.policy --membership alice-relay.membership
```

Distribute the same public `network.policy` and the corresponding device's
membership file to each relay. Never copy an identity keystore between relays.
Start with its existing identity, password and listener arguments plus:

```bash
mesh-talk-node --post-office --name relay --keystore /data/identity.keystore \
  --password-file /data/password --relay-network /data/network.policy \
  --relay-membership /data/relay.membership
```

Network mode enables the relay directory automatically; clients must still opt
into directory discovery themselves. No client federation configuration is
required. Settings show relay availability, which is neither proof of a live
connection nor a software-integrity verdict.

For another reachable site, optionally repeat `--relay-seed
<full-fingerprint>@<ip>:<discovery-udp-port>`. Seeds use the existing signed,
nonce-bound discovery probe; they locate a route and do not replace membership.
Network mode cannot be combined with `--federation-config`, `--federation-peer`
or a legacy `--bridge-peer` gateway. Existing pin mode remains supported separately.
A membership failure closes the exchange without retrying legacy federation.
Normal mailbox clients continue to use the same opaque-only relay protocol and
need no relay membership certificate.

Memberships last 1–90 days (30 by default). Relays check validity on each new
membership exchange; keep clocks synchronized. To renew, issue a certificate to
a new file, deploy it as the relay's membership file, and restart. `init` and
`enroll` never overwrite existing files. Public policy and membership use bounded
versioned binary encoding: manage them with the CLI rather than a text editor.
Policies are at most 64 KiB with 1,024 revoked fingerprints; memberships are at
most 4 KiB. The mutual membership exchange has a 10-second timeout per endpoint
and reuses existing bounded discovery, connection and reconciliation budgets.

Revoke a whole device without the offline authority key:

```bash
mesh-talk-node network revoke --policy network.policy --device aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
```

Distribute the updated public policy and restart **every** affected relay.
Running processes retain their startup policy; editing a file does not terminate
an active session. A revoked fingerprint cannot be renewed under that policy;
replace a compromised device identity and approve its replacement instead.
Revocation cannot erase ciphertext or metadata already copied by an operator.
Policy deployment is a privileged local administrative operation, not an
untrusted peer update. Protect it against replacement and rollback. CLI mutations
use the same persistent companion-file locking and atomic publication as legacy
trust manifests. Authority keys are private regular files (0600 or stricter on
Unix); protect their parent directory and workstation, and apply private ACLs on
Windows. Authority compromise requires a new network authority, replacement
memberships and policy deployment to all participants.

Membership establishes operator approval of an identity. It does **not** establish
an official running binary, malware-free host or trustworthy operator behavior.
Release signatures verify downloaded artifacts; self-reported versions/hashes
can be copied by modified clients. Hardware-backed remote software attestation
is not implemented. See [security boundaries](../SECURITY.md#relay-membership-and-software-integrity).

Focused evidence: `cargo test -p mesh-talk-core --test relay_network --test
relay_network_cli --test federation_config_cli`, the `postoffice::network::` and
`postbox::` library selectors, and `federation_runtime::` binary tests cover
certificate rejection, mutual admission, automatic LAN candidate selection,
legacy compatibility and opaque mailbox transfer on loopback. Installer fixtures
cover public-file persistence without the enrollment key. These are not evidence
of a real service restart or cross-site physical discovery on Windows/macOS.

## One shared public configuration

Building this CLI requires Rust 1.89 or newer, including standard-library file
locking support for concurrent manifest updates.

Create a manifest using the current `mesh-talk-node` binary, without opening a
keystore or providing a password:

```bash
mesh-talk-node federation --file relays.conf init
mesh-talk-node federation --file relays.conf enroll d2c86ed557d201fa877f0ef72dc00a67@192.168.3.16:47478
mesh-talk-node federation --file relays.conf enroll edbbf0daac94d930e687ba86c20d3f06@192.168.3.72:47478
mesh-talk-node federation --file relays.conf enroll edbbf0daac94d930e687ba86c20d3f06@100.82.240.19:47478
mesh-talk-node federation --file relays.conf enroll e2bc2167aab97af10cb893de4ae2fb0c@100.109.255.49:47478
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
edbbf0daac94d930e687ba86c20d3f06@192.168.3.72:47478
edbbf0daac94d930e687ba86c20d3f06@100.82.240.19:47478
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
New directory registrations use a 15-second origin lease; forwarding does not
renew its expiry. Deployed 45-second origin metadata remains accepted for
mixed-version routing compatibility, while new clients cap each public presence
observation at 15 seconds and do not renew repeated identical origin expiry.
These bounds are resource limits, not a measured throughput SLA. See the
[presence contract](network-design-reference.md#person-presence-contract) for
the distinction between retained route hints and current presence.

The relay runtime separately limits seed discovery to eight concurrent probes
and reconciliation to eight concurrent relay sessions. Known trusted LAN routes
are scheduled before routed seed discovery, and successful seed responses are
consumed as they arrive. Routes are deduplicated by endpoint and both public
keys. These relay-runtime budgets are independent of the client Node TCP budget;
large graphs of slow or unreachable peers can still lengthen a reconciliation
round.

## Indexed relay runtime and resource bounds

The dedicated CLI serves plain mailboxes, named bridges/gateways, pinned
federation and approved-network federation through one encrypted indexed store
and shared endpoint. Normal reopen does not load retained ciphertext bodies or
all historical IDs. Legacy import is a separate authenticated migration; the
participant runtime and collecting SDK APIs keep their separate storage behavior.

One endpoint shares four nonwaiting Have reservations, each with 32 MiB logical
accounting, including vector-to-set overlap and retained use. Saturation returns
local retryable overload rather than waiting with partial reservations. The
128 MiB aggregate is a Have accounting budget, not a measured process RSS cap.
Federation staging admits at most 512 outstanding sessions, 262,144 entries and
64 MiB accounted storage across both remote inventory and selected local digests;
current direction limits remain 256 inbound and eight outbound. Stale sessions
remain charged until bounded cleanup. Each federation exchange retains its
absolute 30-second deadline across pages and version attempts. Existing frame,
Have aggregate, round and file-burst limits remain unchanged.

Missing fingerprints request scoped bounded preparation. Queue saturation leaves
an optional cache miss for a later probe; it does not initiate global inventory
preparation. Explicit federation preparation advances in bounded background
passes: up to 512 raw producer rows and, separately, 512 retained IDs hashed.
These limits do not combine the other bounded maintenance classes into one
512-row budget. Empty filtered inventory continuations are not end-of-inventory. Local
readiness, capacity and storage failures retain typed causes in the runtime and
do not trigger older-version negotiation. Only the existing genuine unsupported
protocol outcome permits compatibility fallback; failed network membership never
downgrades to pin-only trust. An earlier valid origin can invalidate a previously
sent view, requiring a subsequent bounded exchange.

The single paced supervisor owns session and staging cleanup, retention, hashes,
discovery and gateway cancellation. It stops admission on faulted storage and
uses the existing service manager's throttled restart policy. Archive-only
cleanup failures retain their cause and an independently paced retry intent;
they cannot block publication staging or normal role, digest and session work.
Identical persistent error diagnostics are not repeatedly rewritten. Initial
publication can leave retryable archive cleanup pending, while provenance and
authoritative database failures remain explicit. A separate physical class
reclaims already-free pages in capped background slices and completes eligible
main-file truncation through PASSIVE checkpoints. It does not delete live mail
or change retention. Reader-pinned or incomplete checkpoints retain independently
paced work; ordinary traffic and cleanup continue. Actual checkpoint work can
exceed the page-step budget, and retained WAL space may remain allocated. The
[storage contract](ARCHITECTURE.md#storage-replay-and-query-budgets) defines
transaction, expiry and shutdown semantics. Functional fast-KDF tests and Unix
joined-stop tests do not establish production unlock performance, native Windows
hard-stop recovery or physical-network discovery acceptance.

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
