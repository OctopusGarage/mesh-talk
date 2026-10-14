# mac2015 post-office service

The Intel Mac at SSH alias `mac2015` runs the headless post office as the
`ys-aquria` user. Its LaunchAgent label is
`com.octopusgarage.mesh-talk.post-office`. The installed binary is
`~/.local/bin/mesh-talk-node`; the relay's identity, account key, and encrypted
event log live in `~/.local/share/mesh-talk-post-office`. Its password is read
from that directory's owner-only `password` file. The password is absent from
the LaunchAgent plist and process arguments. The service listens on TCP 47475
and uses signed UDP multicast discovery on port 47474.

The LaunchAgent plist is at
`~/Library/LaunchAgents/com.octopusgarage.mesh-talk.post-office.plist`.
`RunAtLoad` and `KeepAlive` start and restart it while this user is logged in;
the agent does not run before login or after logout. Logs are under
`~/.local/state/mesh-talk-post-office`.

For a first installation, open an interactive shell on mac2015 and create the
private password file there. Use the same password when creating the keystore.
If the keystore already exists, preserve its original password; replacing this
file with a different password makes the keystore unreadable.

```bash
ssh -t mac2015
umask 077
install -d -m 700 "$HOME/.local/share/mesh-talk-post-office"
read -rs 'relay_password?Post-office keystore password: '
printf '\n'
printf '%s\n' "$relay_password" > "$HOME/.local/share/mesh-talk-post-office/password"
unset relay_password
chmod 600 "$HOME/.local/share/mesh-talk-post-office/password"
stat -f '%Su %Sp %N' "$HOME/.local/share/mesh-talk-post-office" "$HOME/.local/share/mesh-talk-post-office/password"
exit
```

The last command should show the service user as owner, `drwx------` for the
directory, and `-rw-------` for the password file. The prompt reads the secret
without showing it or putting it in a process argument. For recovery, enter the
original keystore password through the same prompt.

## Build and update

Build the Intel binary on a Mac with an installed `x86_64-apple-darwin` Rust
target. Use one toolchain for both Cargo and rustc; mixing Homebrew Cargo with
a rustup target can fail with `can't find crate for core`.

```bash
RUSTC="$HOME/.rustup/toolchains/1.99.0-aarch64-apple-darwin/bin/rustc" \
  "$HOME/.rustup/toolchains/1.99.0-aarch64-apple-darwin/bin/cargo" \
  build --release --target x86_64-apple-darwin -p mesh-talk-core --bin mesh-talk-node
file target/x86_64-apple-darwin/release/mesh-talk-node
scp target/x86_64-apple-darwin/release/mesh-talk-node \
  mac2015:/Users/ys-aquria/.local/bin/mesh-talk-node.next
ssh mac2015 'chmod 755 "$HOME/.local/bin/mesh-talk-node.next"; "$HOME/.local/bin/mesh-talk-node.next" --help >/dev/null'
ssh mac2015 'launchctl bootout gui/$(id -u)/com.octopusgarage.mesh-talk.post-office'
ssh mac2015 'mv "$HOME/.local/bin/mesh-talk-node.next" "$HOME/.local/bin/mesh-talk-node"'
ssh mac2015 'launchctl bootstrap gui/$(id -u) "$HOME/Library/LaunchAgents/com.octopusgarage.mesh-talk.post-office.plist"'
```

Keep the data directory and its password file when updating the binary.
They preserve the relay identity and queued opaque events. The password file
must be a regular file with mode `0600`; the data directory should be mode
`0700`. Back up the password with the keystore if durable custody matters.

## Check and operate

```bash
ssh mac2015 'launchctl print gui/$(id -u)/com.octopusgarage.mesh-talk.post-office | grep -E "state =|pid =|last exit code ="'
ssh mac2015 'lsof -nP -iTCP:47475 -sTCP:LISTEN'
ssh mac2015 'tail -n 20 "$HOME/.local/state/mesh-talk-post-office/stdout.log"'
ssh mac2015 'tail -n 20 "$HOME/.local/state/mesh-talk-post-office/stderr.log"'
ssh mac2015 'launchctl kickstart -k gui/$(id -u)/com.octopusgarage.mesh-talk.post-office'
```

The stdout startup line begins `post-office` and reports the relay device ID
and listening ports. Clients on the same LAN discover it automatically when
post-office use is enabled. If another relay has a lower elected fingerprint,
this node is a standby; queued events are not replicated between relays. The
relay sees connecting device identities, mailbox activity, timing, and size
buckets. See [metadata privacy](post-office-metadata-privacy.md) for the full
trust boundary.

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
checks that the remote relay log grows, stops Alice, and restarts Bob to verify
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
online to approve the offer and answer Alice's consent query. The protocol does
not currently carry that approval through the post office while Alice is absent.

The multi-relay variant starts a disposable second post office on the test Mac,
checks that both peers discover it and mac2015, and confirms that an offline DM
is stored and drained through the lowest-fingerprint elected relay. It removes
the disposable relay when done; queued events do not replicate to the standby.
