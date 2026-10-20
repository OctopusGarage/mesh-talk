# macOS post-office boot service

A post office that must survive a reboot runs as a **LaunchDaemon**, with
`RunAtLoad` and `KeepAlive`. The older LaunchAgent runs only in the logged-in
user's GUI session: it does not run before login or after logout and does not
satisfy unattended boot. The daemon plist is owned by
root, but `UserName` runs the node as the ordinary service user. The encrypted
keystore, password file, and relay log remain in
`~/.local/share/mesh-talk-post-office`; logs are in
`~/.local/state/mesh-talk-post-office`. Never replace an existing password
file or keystore during an update.

The service must use `--password-file`; never put the keystore password in
`--password` process arguments or in a plist. The installer sets the data
directory to mode `700` and its password file to owner-only mode `600`.

## Build, install, and update

From the desired source revision on a Mac, build the Intel release binary with
the same stable toolchain for Cargo and rustc:

```bash
rustup target add --toolchain stable x86_64-apple-darwin
RUSTC="$(rustup which --toolchain stable rustc)" \
  "$(rustup which --toolchain stable cargo)" build --locked --release \
  --target x86_64-apple-darwin -p mesh-talk-core --bin mesh-talk-node
file target/x86_64-apple-darwin/release/mesh-talk-node
```

For Apple Silicon, use `aarch64-apple-darwin` for both the target and artifact
path. The repository's `scripts/install-post-office-macos.sh` has two phases:

1. As the service user, run `prepare` with a staged binary, relay name and
   ports. It verifies the binary, preserves existing identity and queued mail,
   creates a password only for a new relay, and writes a private staged plist.
2. Interactively run `activate` on that Mac. It asks for administrator
   authorization, installs a root-owned plist in `/Library/LaunchDaemons`,
   stops the old per-login agent if present, and starts the boot service.

Managed post offices use dedicated UDP **47478** on every host and also
participate on shared LAN UDP **47474**. The installer supplies both defaults
and fixes the main TCP listener at **47479**. Preparation and activation reject
other main TCP or discovery ports before changing service state. Desktop
apps use UDP 47474; routed inventories probe both role ports. Different hosts
are distinguished by IP, so their post offices do not need different ports.
The raw CLI permits custom ports for isolated test networks; those endpoints
require explicit discovery and are outside the managed deployment contract.
The signed discovery response advertises TCP 47479. Optional legacy gateways
use a separate TCP listener; their port must differ from the main listener.

Example for the Intel relay on LAN X (paths and addresses are deployment-specific):

```bash
ssh mac2015 'install -d -m 700 "$HOME/.local/state/mesh-talk-post-office"'
scp target/x86_64-apple-darwin/release/mesh-talk-node scripts/install-post-office-macos.sh relays.conf mac2015:~/.local/state/mesh-talk-post-office/
ssh mac2015 'bash ~/.local/state/mesh-talk-post-office/install-post-office-macos.sh prepare \
  --binary ~/.local/state/mesh-talk-post-office/mesh-talk-node \
  --name mac2015-post-office --port 47479 --discovery-port 47478 \
  --lan-announce-port 47474 --bridge \
  --federation-config ~/.local/state/mesh-talk-post-office/relays.conf'
ssh -tt mac2015 'bash ~/.local/state/mesh-talk-post-office/install-post-office-macos.sh activate'
```

For an update, repeat the build, transfer, `prepare`, and `activate` commands
with the same service user, ports, name, and trust manifest. `prepare` installs
and validates `~/.local/bin/mesh-talk-node.next`, then replaces the binary with
`mv`; `activate` restarts the daemon. It preserves the existing password,
keystore, and queued relay data. If existing state has no password file, recover
the original password instead of creating a new relay identity.

`prepare` also preserves the installed executable's code-signing identifier
when updating an ad hoc or unsigned local build. A first local installation uses
`com.octopusgarage.mesh-talk.node`. Publisher-issued signatures are preserved,
and signature verification must succeed before the installed executable is
replaced. The code-signing identifier is separate from the cryptographic relay
device ID: both need continuity, but preserving the keystore alone does not
preserve macOS's local-network permission tracking. See Apple's
[local network privacy guidance](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy).

Use `prepare` for updates rather than copying a freshly linked executable over
the installed binary. After restarting, check actual LAN discovery as well as
the TCP listener and routed signed query. A running service or a successful
Tailscale connection does not prove that LAN multicast is working. If an older
manual update changed the signing identifier, preserve the identifier from the
known working binary backup when preparing the repair; do not disable
system-wide local-network privacy.

Example for the reciprocal relay on another LAN:

```bash
scp target/aarch64-apple-darwin/release/mesh-talk-node mac2015:~/.local/state/mesh-talk-post-office/mesh-talk-node-arm
ssh mac2015 'scp ~/.local/state/mesh-talk-post-office/mesh-talk-node-arm mac-mini:/Users/stella/.local/state/mesh-talk-post-office/'
ssh mac2015 'scp ~/.local/state/mesh-talk-post-office/install-post-office-macos.sh ~/.local/state/mesh-talk-post-office/relays.conf mac-mini:~/.local/state/mesh-talk-post-office/'
ssh -tt mac2015 'ssh -tt mac-mini bash /Users/stella/.local/state/mesh-talk-post-office/install-post-office-macos.sh prepare \
  --binary /Users/stella/.local/state/mesh-talk-post-office/mesh-talk-node-arm \
  --name mac-mini-post-office --port 47479 --discovery-port 47478 \
  --lan-announce-port 47474 --bridge \
  --federation-config /Users/stella/.local/state/mesh-talk-post-office/relays.conf'
ssh -tt mac2015 'ssh -tt mac-mini bash /Users/stella/.local/state/mesh-talk-post-office/install-post-office-macos.sh activate'
```

Both commands use the same public manifest; each host keeps its own private
identity and listener configuration. Preparation validates the manifest and
copies it to `~/.local/share/mesh-talk-post-office/federation.conf`; the daemon
uses that persistent absolute path. Enrollment/revocation changes require
redistributing the updated file and restarting every affected relay. A file edit
alone does not revoke running sessions. Existing `--federation-peer` flags remain
supported when no manifest is supplied.

If the only physical route between two sites fails, the local relay retains
encrypted outgoing mail within its retention and capacity limits until a route returns.

## Migrate the legacy LaunchAgent

The old job is `gui/<service-user-uid>/com.octopusgarage.mesh-talk.post-office`;
its plist is
`~/Library/LaunchAgents/com.octopusgarage.mesh-talk.post-office.plist`.
Inspect it as the service user in that user's logged-in GUI session:

```bash
launchctl print "gui/$(id -u)/com.octopusgarage.mesh-talk.post-office" 2>/dev/null || true
if [ -f "$HOME/Library/LaunchAgents/com.octopusgarage.mesh-talk.post-office.plist" ]; then
  plutil -lint "$HOME/Library/LaunchAgents/com.octopusgarage.mesh-talk.post-office.plist"
fi
```

`prepare` writes `~/.local/state/mesh-talk-post-office/LaunchDaemon.plist`;
`activate` installs it at
`/Library/LaunchDaemons/com.octopusgarage.mesh-talk.post-office.plist` and
unloads the old GUI job. After verifying the system job below, remove the old
agent plist so the next GUI login cannot load a competing relay:

```bash
launchctl bootout "gui/$(id -u)/com.octopusgarage.mesh-talk.post-office" 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/com.octopusgarage.mesh-talk.post-office.plist"
```

## Check and operate

Check boot-service state, credentials, and listeners as the service user:

```bash
sudo launchctl print system/com.octopusgarage.mesh-talk.post-office
lsof -nP -iTCP:47479 -sTCP:LISTEN
lsof -nP -iUDP:47478
shasum -a 256 ~/.local/bin/mesh-talk-node
chmod 600 "$HOME/.local/share/mesh-talk-post-office/password"
stat -f '%Su %Sp %N' "$HOME/.local/share/mesh-talk-post-office" "$HOME/.local/share/mesh-talk-post-office/password"
```

The owner must be the service user; permissions must be `drwx------` on the
data directory and `-rw-------` on the password file. The daemon plist contains
the password file's absolute path, never the secret. To restart the installed
service after an update or configuration change:

```bash
sudo launchctl kickstart -k system/com.octopusgarage.mesh-talk.post-office
sudo launchctl print system/com.octopusgarage.mesh-talk.post-office
```

The LaunchDaemon restarts the process after a crash and after boot, before a
user logs in. Tailscale availability is separate: a relay may start before its
VPN is ready, and its federation loop retries until the routed path returns. To
verify an actual reboot, restart one machine at a time and recheck the system
launchd job, signed discovery, configured listeners, and an encrypted message.

Relays see authenticated device/IP connections, pseudonymous mailbox and event
IDs, timing, counts, and size buckets; trusted federation peers can correlate
the same mailbox across sites. Legacy relay-specific queues do not replicate.
Version-3 federation reconciles reachable trusted relays opportunistically;
using several relays does not guarantee replicated custody or delivery after a
route loss. See [relay metadata privacy](post-office-metadata-privacy.md) for
trust, retention, and deletion limits.

## Cross-machine delivery test

From a Mac on the same LAN, build the CLI with `cargo build -p mesh-talk-core
--bin mesh-talk-node`, take the relay device ID from its startup line, then run:

```bash
python3 scripts/e2e-post-office-lan.py --ssh-host mac2015 --relay-id <relay-device-id>
python3 scripts/e2e-post-office-lan.py --ssh-host mac2015 --relay-id <relay-device-id> --file-roundtrip
python3 scripts/e2e-post-office-lan.py --ssh-host mac2015 --relay-id <relay-device-id> --multi-relay
```

The test creates disposable Alice and Bob identities, checks discovery of the
installed relay, sends a DM and a private channel event while Bob is offline,
stops Alice, and restarts Bob to verify
both deliveries. It leaves the installed relay identity and queued data intact.
The file roundtrip variant proves that the offer reaches Bob through the
installed relay after Alice stops. This older harness queries Bob's local
attachment consent when Alice returns and transfers the bytes while both peers
are online; that overlap is a harness limitation, not a current protocol
requirement. The Python file roundtrip does not prove that file bytes crossed
the relay.

The Python `--restart-relay-before-drain` switch restarts the legacy
`gui/<uid>/...` job. The ignored mac2015 Rust test also defaults to that job;
select `MESH_TALK_E2E_MAC2015_SERVICE=system` for the current LaunchDaemon.
Only `system` and `legacy-agent` are accepted selector values.

```bash
python3 scripts/e2e-post-office-lan.py --ssh-host mac2015 --relay-id <relay-device-id> --restart-relay-before-drain
MESH_TALK_E2E_MAC2015_RELAY_ID=<relay-device-id> cargo test -p mesh-talk-core --lib mac2015_post_office_delivers_file_bytes_after_both_peers_go_offline -- --ignored --nocapture
```

The ignored Rust LAN test covers that final handoff. It discovers the signed
announcement from the installed mac2015 relay, keeps Bob offline while Alice
uploads the file offer, then drains the offer and obtains Bob's authenticated
consent. It stops Bob before any chunks arrive, uploads the encrypted chunks to
mac2015, stops Alice, restarts the relay, and checks that Bob recovers the exact
file bytes. This is a deliberate test harness sequence: Bob must briefly be
online to approve the offer and answer Alice's direct consent query.

For the installed **system LaunchDaemon**, run the same Rust test explicitly
in system mode. Its SSH restart uses `sudo -n`; the SSH service user must already
have administrator authorization permitting that command noninteractively.
An interactive `sudo -v` in another SSH session does not necessarily authorize
this session. Confirm the site's approved sudo policy first; this test neither
prompts for nor transmits an administrator password. It restarts the installed
relay, so run it in an agreed service-maintenance window:

```bash
MESH_TALK_E2E_MAC2015_SERVICE=system \
MESH_TALK_E2E_MAC2015_RELAY_ID=<relay-device-id> \
  cargo test -p mesh-talk-core --lib \
  node::delivery_runtime_tests::mac2015_post_office_delivers_file_bytes_after_both_peers_go_offline \
  -- --ignored --exact --nocapture
```

The test authenticates consent for the exact offer over Noise, closes Bob's
listener, and requires the relay to confirm durable custody of every encrypted
chunk within 30 seconds before dropping Alice. Custody must still leave sender
completion false. It restarts the system job, then requires Bob to recover the
exact bytes within 30 seconds with Alice absent, saves the file, compares its
SHA-256, and checks the recipient's durable completion record bound to Alice
and the exact final chunk. Completion is observed at the recipient while Alice
remains stopped; this is **not** a sender-observed delivery confirmation or a
test of federated return controls. The command is an executable acceptance
procedure, not evidence that it has been run against a particular installation.

The current protocol carries signed, exact-offer consent and verified-save
completion through version-3 encrypted federated opaque mailboxes. Sender and
recipient need no direct route or simultaneous online overlap for those
controls; each must eventually reach an eligible relay, and the sender uploads
chunks after learning authenticated consent. Delivery still depends on
reachable federation and retained data. With the debug binaries and SSH
fixtures prepared as described in `scripts/e2e-four-machine-tailnet.py`, the
installed-federation mode verifies cross-relay file bytes and SHA-256:

```bash
MESH_TALK_E2E_INSTALLED_RELAYS=1 python3 scripts/e2e-four-machine-tailnet.py
```

For an executable check of authenticated federated consent and sender-observed
completion, build the `network-acceptance` and `mesh-talk-node` debug binaries
for the hosts listed in the recovery script's header, then run:

```bash
python3 scripts/e2e-federation-recovery.py
```

This rig uses disposable relays, leaving installed services untouched. It stops
the preferred relay, queues a 130,000-byte attachment through the surviving
R→Q→S route, restarts the sender before recipient approval, waits for verified
file readiness, saves the recipient's file, compares SHA-256, and requires the
sender's `file-sent <card-id>` query to return `file-sent true`. That last check
observes authenticated recipient completion; a successful relay upload alone
cannot satisfy it. The rig subsequently stops and restarts the intermediary
relay to test recovery of a separate queued DM.

The installed-federation rig does not restart the system service or assert
sender-observed completion. The recovery rig checks sender completion, but its
intermediary restart occurs after that file completes and uses a disposable
process. Keep these evidence boundaries separate from the system-service Rust
procedure above; combining results does not prove an unobserved lifecycle.

The multi-relay variant starts a disposable second post office on the test Mac,
checks that both peers discover it and mac2015, and confirms that an offline DM
is stored and drained through the automatically ordered eligible relay. This
legacy mailbox-only variant removes its disposable relay and does not configure
federation, so its result does not establish replicated custody. Current equal
bridge relays use the shared trust manifest described in
[the federation guide](post-office-federation.md).
