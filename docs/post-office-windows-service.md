# Windows post-office service

The machine reached through SSH alias `windows10` currently runs Windows 11
Home (build 26200). Its `MeshTalk-PostOffice` Scheduled Task runs the headless
relay as `SYSTEM` at boot. The binary is
`D:\ProgramData\MeshTalk\PostOffice\mesh-talk-node.exe`; its keystore, password
file, `relay.log`, and `node.lock` are in the `data` subdirectory. TCP 47479
serves peers; its dedicated signed-query UDP port is 47478 (the managed post-office port on every platform) and it announces
to clients on the shared LAN port 47474. The optional legacy gateway listens
on TCP 47476. The equal-relay federation uses the same trusted seed list as
mac2015 and mac-mini. The installer allows inbound TCP 47479, UDP 47478 and
the optional gateway TCP port from `LocalSubnet` on all network profiles.
Shared-port LAN announcements on UDP 47474 depend on the host's existing
firewall policy.

The task passes `--password-file`, never a password argument. The installer
creates a random password on the Windows host only on first installation and
restricts the relay directory to `SYSTEM` and local `Administrators`. Preserve
the password file and keystore together: replacing either changes or locks out
the relay identity. The installer refuses to generate a new password when it
finds existing relay state without the original password file. Back up the
whole data directory if durable custody matters.

## Build, install, and update

On an Apple Silicon Mac with the project-compatible stable Rust toolchain and MinGW installed:

```bash
rustup target add --toolchain stable x86_64-pc-windows-gnu
RUSTC="$(rustup which --toolchain stable rustc)" \
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo \
  build --release --target x86_64-pc-windows-gnu \
  -p mesh-talk-core --bin mesh-talk-node
scp target/x86_64-pc-windows-gnu/release/mesh-talk-node.exe \
  windows10:'D:/mesh-talk-node.next.exe'
scp scripts/install-post-office-windows.ps1 \
  windows10:'D:/install-post-office-windows.ps1'
scp relays.conf windows10:'D:/mesh-talk-relays.conf'
```

Then run this in an elevated PowerShell session on Windows (working directory
D:). For updates, stop the previous relay and verify it exited using the
procedure below before running the installer.

```powershell
& 'D:\install-post-office-windows.ps1' `
  -StagedBinary 'D:\mesh-talk-node.next.exe' `
  -Name 'windows10-post-office' `
  -DiscoveryPort 47478 -LanAnnouncePort 47474 `
  -FederationConfig 'D:\mesh-talk-relays.conf'
```

Create `relays.conf` with the [shared federation workflow](post-office-federation.md)
and distribute the same public pins to every relay. The installer validates it
before changes and persists it at
`D:\ProgramData\MeshTalk\PostOffice\data\federation.conf`. Keep both LAN and
Tailscale endpoints for a relay where appropriate; each pins the same identity.
`-FederationConfig` conflicts with inline `-FederationPeer` entries. Enrollment
and revocation require restarting all affected services after distribution;
editing a file does not hot-revoke a running connection.

For automatic member discovery, replace `-FederationConfig` with
`-RelayNetwork 'D:\staging\network.policy' -RelayMembership
'D:\staging\relay.membership'`, optionally repeating routed seeds through the
`-RelaySeed` array. Issue the device-specific membership on the offline
workstation using [network administration](post-office-federation.md#automatic-membership-in-an-approved-network).
No explicit `-Bridge` is needed. The installer validates public approval before
mutation and persists `network.policy` and `relay.membership` in the restricted
service data directory. Startup additionally checks the membership matches the
loaded persistent identity before binding listeners. Preserve the existing
keystore/password, never install the authority private key, and rerun the
installer to restart after public policy deployment or membership renewal.
Network mode conflicts with legacy federation pins and bridge gateways.

The installer fixes the main TCP listener at 47479 and enforces
`-DiscoveryPort 47478 -LanAnnouncePort 47474` and rejects
other discovery ports before changing service state. Different hosts use the
same role ports, distinguished by IP.

`-LanAnnouncePort 47474` also listens on the shared LAN discovery port. The
dedicated port remains available for routed signed queries when shared-port
multicast reaches only one local process.

Run those commands for a first install and for each update. The installer
advertises the name `post-office` by default; pass `-Name <your-relay-name>`
to choose a distinct LAN display name. On first install,
the script creates the random password and the first task start creates the
keystore; on update, it stops an
existing task, replaces only the binary, checks ACLs, registers the boot task,
starts it, and waits for the TCP listener and, when configured, the bridge
proxy. It preserves the data directory and identity. `-Bridge` alone enables a
local directory; `-BridgePeer` plus `-BridgePeerId` authenticate a signed remote relay
and enable the gateway. The manifest names mutually trusted equals. Participating clients enable bridge
discovery and choose an eligible directly reachable preferred post office in Settings. If it is offline, another
verified local post office can accept messages. The CLI holds an exclusive OS lock for the data directory and refuses another
cooperating updated CLI owner. The `node.lock` file remains after exit; its PID
is diagnostic information, not the lock itself. See
[data directory ownership](../specifications/deployment_guide.md#data-directory-ownership)
for the required quiescent upgrade and mixed-version limits.

## Check and operate

```bash
ssh windows10 'powershell -NoProfile -Command "(Get-ScheduledTask -TaskName MeshTalk-PostOffice).State; (Get-ScheduledTaskInfo -TaskName MeshTalk-PostOffice).LastTaskResult; Get-NetTCPConnection -State Listen -LocalPort 47479,47476 | Select-Object LocalAddress,LocalPort,OwningProcess"'
ssh windows10 'powershell -NoProfile -Command "(Get-ScheduledTask -TaskName MeshTalk-PostOffice).Principal.UserId; icacls D:\ProgramData\MeshTalk\PostOffice\data\password; icacls D:\ProgramData\MeshTalk\PostOffice\data\identity.keystore"'
```

The task state should be `Running`, and TCP 47479 should have a listener.
The task principal must be `SYSTEM`; the password and keystore ACLs must grant
only `SYSTEM` and `Administrators`. Check the listener's owning PID and executable path with
`Get-Process -Id <PID> | Select-Object Id,Path`. Windows can prevent reading
`node.lock` through another handle while the exclusive lock is held; use the
listener and process inventory to identify the live owner.
`267009` (`0x41301`) is Task Scheduler's running result. The startup output
reports the relay device ID; signed discovery can also verify that ID from a
different LAN node. If a task fails, inspect Task Scheduler history and the
data directory's access permissions before changing the keystore or password.

### Stop or restart the installed relay

Run this from elevated PowerShell. Stop the task first and give its process up
to 30 seconds to exit. A timeout stops the procedure before a restart or binary
replacement.

```powershell
$task = 'MeshTalk-PostOffice'
$binary = 'D:\ProgramData\MeshTalk\PostOffice\mesh-talk-node.exe'
Stop-ScheduledTask -TaskName $task -ErrorAction Stop
$deadline = (Get-Date).AddSeconds(30)
while (Get-CimInstance Win32_Process -Filter "Name = 'mesh-talk-node.exe'" -ErrorAction Stop |
    Where-Object { $_.ExecutablePath -eq $binary }) {
    if ((Get-Date) -ge $deadline) { throw "Previous relay process did not exit" }
    Start-Sleep -Milliseconds 500
}
```

For a standalone restart, run `Start-ScheduledTask -TaskName $task` after the stop
check. For an update, run the installer from the preceding section. Wait for the
task to be `Running` and verify that the TCP 47479 listener belongs to the
installed executable. Confirm the same signed relay device ID from another LAN
node.

After an abrupt stop, the OS releases the terminated process's lock while
`node.lock` remains. Keep the file and reuse the existing data directory,
password and keystore to preserve queued events and relay identity.

## Cross-machine tests

Build the local CLI with `cargo build -p mesh-talk-core --bin mesh-talk-node`,
then run the tests from a second machine on the same LAN. Supply the installed
relay's signed device ID for `<relay-id>`:

```bash
python3 scripts/e2e-post-office-lan.py --ssh-host windows10 --remote-os windows --relay-id <relay-id> --restart-relay-before-drain
python3 scripts/e2e-post-office-lan.py --ssh-host windows10 --remote-os windows --relay-id <relay-id> --file-roundtrip --restart-relay-before-drain
python3 scripts/e2e-post-office-lan.py --ssh-host windows10 --remote-os windows --relay-id <relay-id> --multi-relay
MESH_TALK_E2E_WINDOWS10_RELAY_ID=<relay-id> cargo test -p mesh-talk-core --lib windows10_post_office_delivers_file_bytes_after_both_peers_go_offline -- --ignored --nocapture
```

Each command targets the installed Windows relay by its signed device ID.
The disposable recipient verifies the sender's full device fingerprint before
going offline, as required for automatic relay recovery.
The first command separately asserts receipt of the offline DM and the private
channel event after the sender stops and the relay restarts. The second checks
an offline file offer, then an exact byte roundtrip after the sender returns.
The third command checks election against a disposable second relay. The
LAN script uses relay-log growth as a historical pre-drain signal.
Treat that signal as a fixture prerequisite and use the recipient's history
or saved file as delivery evidence. Indexed storage can keep active state
outside the legacy log, so a log-growth timeout alone cannot diagnose custody.

The ignored Rust test requires custody evidence for every encrypted chunk
while both peers are offline, then compares recovered bytes and the saved file
hash with the sender stopped. Its task stop/start command uses a fixed delay.
To establish that a replacement service served the bytes, record the old
listener process exiting and the new listener's executable and signed relay
identity using the stop and restart procedure above. The file fixtures obtain
consent during brief direct sender-recipient overlap as part of their setup.

Current bridge clients return signed, exact-offer consent and verified-save
completion through version-3 encrypted federated opaque mailboxes. Those
controls need no direct sender-recipient route or simultaneous online overlap.
An eligible relay can take custody of encrypted chunks before consent.
Recipient approval authorizes retrieval for the exact offer, and a verified
save produces a separate signed completion. Delivery depends on accessible
trusted federation and retained data. The installed-federation mode in
`scripts/e2e-four-machine-tailnet.py` verifies the saved bytes and SHA-256
with the mac2015 and mac-mini relays. Windows indexed federation needs its
own installed-path evidence.

Routing metadata remains exposed to the relay, including authenticated device
connections, mailbox activity, timing, and size buckets. See
[relay metadata privacy](post-office-metadata-privacy.md) for the exact fields.
Legacy relay-specific mailboxes do not replicate. Version-3 federation can
copy opaque events among reachable trusted relays, but this opportunistic
reconciliation does not guarantee replicated custody. Federation extends
mailbox correlation to those trusted peers; unreachable paths, retention,
eviction, or relay withholding/deletion can still prevent delivery. The privacy
guide also explains why clients cannot guarantee remote removal from every
relay queue.
