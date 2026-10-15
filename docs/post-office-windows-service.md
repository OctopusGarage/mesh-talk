# Windows post-office service

The machine reached through SSH alias `windows10` currently runs Windows 11
Home (build 26200). Its `MeshTalk-PostOffice` Scheduled Task runs the headless
relay as `SYSTEM` at boot. The binary is
`D:\ProgramData\MeshTalk\PostOffice\mesh-talk-node.exe`; its keystore, password
file, `relay.log`, and `node.lock` are in the `data` subdirectory. TCP 47475
serves peers; its dedicated signed-query UDP port is 47478 and it announces
to clients on the shared LAN port 47474. The optional legacy gateway listens
on TCP 47476. The equal-relay federation uses the same trusted seed list as
mac2015 and mac-mini. Firewall rules allow the relay listener ports from
`LocalSubnet` on all network profiles.

The task passes `--password-file`, never a password argument. The installer
creates a random password on the Windows host only on first installation and
restricts the relay directory to `SYSTEM` and local `Administrators`. Preserve
the password file and keystore together: replacing either changes or locks out
the relay identity. The installer refuses to generate a new password when it
finds existing relay state without the original password file. Back up the
whole data directory if durable custody matters.

## Build, install, and update

On an Apple Silicon Mac with Rust 1.99 and MinGW installed:

```bash
rustup target add --toolchain 1.99.0-aarch64-apple-darwin x86_64-pc-windows-gnu
RUSTC="$HOME/.rustup/toolchains/1.99.0-aarch64-apple-darwin/bin/rustc" \
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  "$HOME/.rustup/toolchains/1.99.0-aarch64-apple-darwin/bin/cargo" \
  build --release --target x86_64-pc-windows-gnu \
  -p mesh-talk-core --bin mesh-talk-node
scp target/x86_64-pc-windows-gnu/release/mesh-talk-node.exe \
  windows10:'D:/mesh-talk-node.next.exe'
scp scripts/install-post-office-windows.ps1 \
  windows10:'D:/install-post-office-windows.ps1'
```

Then run this in a PowerShell session on Windows (working directory D:):

```powershell
& 'D:\install-post-office-windows.ps1' `
  -StagedBinary 'D:\mesh-talk-node.next.exe' `
  -Name 'windows10-post-office' `
  -BridgePeer '192.168.3.72:47479' `
  -BridgePeerId 'edbbf0daac94d930e687ba86c20d3f06' `
  -BridgeProxyPort 47476 -DiscoveryPort 47478 -LanAnnouncePort 47474 `
  -FederationPeer @(
    'd2c86ed557d201fa877f0ef72dc00a67@192.168.3.16:47478',
    'edbbf0daac94d930e687ba86c20d3f06@100.82.240.19:47479',
    'e2bc2167aab97af10cb893de4ae2fb0c@100.109.255.49:47480'
  )
```

Run those commands for a first install and for each update. The installer
advertises the name `post-office` by default; pass `-Name <your-relay-name>`
to choose a distinct LAN display name. On first install,
the script creates the random password and the first task start creates the
keystore; on update, it stops an
existing task, replaces only the binary, checks ACLs, registers the boot task,
starts it, and waits for the TCP listener and, when configured, the bridge
proxy. It preserves the data directory and identity. `-Bridge` alone enables a
local directory; `-BridgePeer` plus `-BridgePeerId` authenticate a signed remote relay
and enable the gateway. The example points to mac2015's dedicated routed
discovery port. Participating clients enable bridge discovery and can choose
mac2015 as their preferred post office in Settings. If it is offline, another
verified local post office can accept messages. The CLI reclaims a `node.lock` from a
terminated process on Windows; it refuses a lock held by a live process.

## Check and operate

```bash
ssh windows10 'powershell -NoProfile -Command "(Get-ScheduledTask -TaskName MeshTalk-PostOffice).State; (Get-ScheduledTaskInfo -TaskName MeshTalk-PostOffice).LastTaskResult; Get-NetTCPConnection -State Listen -LocalPort 47475,47476 | Select-Object LocalAddress,LocalPort,OwningProcess"'
ssh windows10 'powershell -NoProfile -Command "(Get-ScheduledTask -TaskName MeshTalk-PostOffice).Principal.UserId; icacls D:\ProgramData\MeshTalk\PostOffice\data\password; icacls D:\ProgramData\MeshTalk\PostOffice\data\identity.keystore"'
ssh windows10 'powershell -NoProfile -Command "Get-Content D:\ProgramData\MeshTalk\PostOffice\data\node.lock"'
ssh windows10 'powershell -NoProfile -Command "Stop-ScheduledTask -TaskName MeshTalk-PostOffice; Start-Sleep -Seconds 2; Start-ScheduledTask -TaskName MeshTalk-PostOffice"'
```

The task state should be `Running`, and TCP 47475 should have a listener.
The task principal must be `SYSTEM`; the password and keystore ACLs must grant
only `SYSTEM` and `Administrators`. The listener's owning PID should match the
PID in `node.lock`.
`267009` (`0x41301`) is Task Scheduler's running result. The startup output
reports the relay device ID; signed discovery can also verify that ID from a
different LAN node. If a task fails, inspect Task Scheduler history and the
data directory's access permissions before changing the keystore or password.

After an abrupt task stop, a remaining `node.lock` can be stale. Read its PID
using the command above, then check `Get-Process -Id <PID>` on Windows. If that
process is gone, start the task again; the CLI reclaims the stale lock itself.
If the process is still running, do not remove the lock or start a second node
against the same data directory. This recovery keeps queued events and the
relay identity intact.

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
The ignored Rust test separately proves that encrypted file chunks themselves
survive on the installed Windows relay while both peers are offline, then
arrive intact after a relay restart with the sender stopped. Recipient consent
still needs a brief direct connection to the sender before those chunks are
uploaded. The third command checks election against a disposable second relay.

Routing metadata remains exposed to the relay, including authenticated device
connections, mailbox activity, timing, and size buckets. See
[relay metadata privacy](post-office-metadata-privacy.md) for the exact fields.
Other relays do not replicate custody; only the elected relay has its queued
events.
