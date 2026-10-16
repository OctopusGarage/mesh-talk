# macOS post-office boot service

A post office that must survive a reboot runs as a **LaunchDaemon**, with
`RunAtLoad` and `KeepAlive`. A LaunchAgent only starts after its user logs in;
that older setup does not satisfy unattended boot. The daemon plist is owned by
root, but `UserName` runs the node as the ordinary service user. The encrypted
keystore, password file, and relay log remain in
`~/.local/share/mesh-talk-post-office`; logs are in
`~/.local/state/mesh-talk-post-office`. Never replace an existing password
file or keystore during an update.

Build `mesh-talk-node` for the target Mac architecture from the desired source
revision. The repository's `scripts/install-post-office-macos.sh` has two
phases:

1. As the service user, run `prepare` with a staged binary, relay name and
   ports. It verifies the binary, preserves existing identity and queued mail,
   creates a password only for a new relay, and writes a private staged plist.
2. Interactively run `activate` on that Mac. It asks for administrator
   authorization, installs a root-owned plist in `/Library/LaunchDaemons`,
   stops the old per-login agent if present, and starts the boot service.

For a relay, use `--bridge` and a dedicated discovery port if another Mesh Talk
process on the same host already owns UDP 47474. In that case, also set
`--lan-announce-port 47474`: the relay keeps receiving routed queries on its
dedicated port and also listens and advertises on the shared LAN port. This
lets it discover other local relays and lets clients use it when another post
office on the LAN is offline.
For equal relays across LANs, create and distribute the same public trust file
using the [federation operator workflow](post-office-federation.md). Routed UDP
discovery and TCP must be permitted over LAN/Tailscale. The optional older
`--bridge-peer` gateway remains a migration mechanism; equal federation does
not require a designated hub or a TCP proxy port.

Example for the Intel relay on LAN X (paths and addresses are deployment-specific):

```bash
scp target/x86_64-apple-darwin/release/mesh-talk-node scripts/install-post-office-macos.sh relays.conf mac2015:~/.local/state/mesh-talk-post-office/
ssh mac2015 'bash ~/.local/state/mesh-talk-post-office/install-post-office-macos.sh prepare \
  --binary ~/.local/state/mesh-talk-post-office/mesh-talk-node \
  --name mac2015-post-office --port 47475 --discovery-port 47479 \
  --lan-announce-port 47474 --bridge \
  --federation-config ~/.local/state/mesh-talk-post-office/relays.conf'
ssh -tt mac2015 'bash ~/.local/state/mesh-talk-post-office/install-post-office-macos.sh activate'
```

Example for the reciprocal relay on another LAN:

```bash
scp target/aarch64-apple-darwin/release/mesh-talk-node mac2015:~/.local/state/mesh-talk-post-office/mesh-talk-node-arm
ssh mac2015 'scp ~/.local/state/mesh-talk-post-office/mesh-talk-node-arm mac-mini:/Users/stella/.local/state/mesh-talk-post-office/'
ssh mac2015 'scp ~/.local/state/mesh-talk-post-office/install-post-office-macos.sh ~/.local/state/mesh-talk-post-office/relays.conf mac-mini:~/.local/state/mesh-talk-post-office/'
ssh -tt mac2015 'ssh -tt mac-mini bash /Users/stella/.local/state/mesh-talk-post-office/install-post-office-macos.sh prepare \
  --binary /Users/stella/.local/state/mesh-talk-post-office/mesh-talk-node-arm \
  --name mac-mini-post-office --port 47476 --discovery-port 47480 \
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
encrypted outgoing mail until a route returns.

Check boot-service state and listeners:

```bash
sudo launchctl print system/com.octopusgarage.mesh-talk.post-office
lsof -nP -iTCP:47475 -sTCP:LISTEN
lsof -nP -iUDP:47479
shasum -a 256 ~/.local/bin/mesh-talk-node
```

The LaunchDaemon restarts the process after a crash and after boot, before a
user logs in. Tailscale availability is separate: a spoke may start before its
VPN is ready, and its gateway loop retries until the routed path returns. To
verify an actual reboot, restart one machine at a time and recheck the system
launchd job, signed discovery, proxy listener and an encrypted message.

## Cross-machine delivery test

From a Mac on the same LAN, build the CLI with `cargo build -p mesh-talk-core
--bin mesh-talk-node`, take the relay device ID from its startup line, then run:

```bash
python3 scripts/e2e-post-office-lan.py --ssh-host mac2015 --relay-id <relay-device-id>
python3 scripts/e2e-post-office-lan.py --ssh-host mac2015 --relay-id <relay-device-id> --restart-relay-before-drain
python3 scripts/e2e-post-office-lan.py --ssh-host mac2015 --relay-id <relay-device-id> --file-roundtrip --restart-relay-before-drain
python3 scripts/e2e-post-office-lan.py --ssh-host mac2015 --relay-id <relay-device-id> --multi-relay
MESH_TALK_E2E_MAC2015_RELAY_ID=<relay-device-id> cargo test -p mesh-talk-core --lib mac2015_post_office_delivers_file_bytes_after_both_peers_go_offline -- --ignored --nocapture
```

The test creates disposable Alice and Bob identities, checks discovery of the
installed relay, sends a DM and a private channel event while Bob is offline,
stops Alice, and restarts Bob to verify
both deliveries. It leaves the installed relay identity and queued data intact.
The file roundtrip variant proves that the offer reaches Bob through the
installed relay after Alice stops, including across a relay restart. Bob's
attachment consent is local state queried by Alice when she returns; file
content transfer then completes while both peers are online. This test does
not prove that the file content itself crossed the relay.

The ignored Rust LAN test covers that final handoff. It discovers the signed
announcement from the installed mac2015 relay, keeps Bob offline while Alice
uploads the file offer, then drains the offer and obtains Bob's authenticated
consent. It stops Bob before any chunks arrive, uploads the encrypted chunks to
mac2015, stops Alice, restarts the relay, and checks that Bob recovers the exact
file bytes. This is a deliberate test harness sequence: Bob must briefly be
online to approve the offer and answer Alice's consent query. This older rig uses a direct consent query. The current protocol also carries
signed approval and verified-save completion through encrypted federated mailboxes;
`scripts/e2e-four-machine-tailnet.py` in installed-federation mode verifies the
cross-relay file bytes and SHA-256 separately.

The multi-relay variant starts a disposable second post office on the test Mac,
checks that both peers discover it and mac2015, and confirms that an offline DM
is stored and drained through the lowest-fingerprint elected relay. It removes
the disposable relay when done; queued events do not replicate to the standby.
