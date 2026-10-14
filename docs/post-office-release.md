# Post-office release package

Mesh-Talk release tags produce a separate `mesh-talk-post-office_<tag>_<os>_<arch>.zip`
for each desktop release platform. These archives contain the headless
`mesh-talk-node` binary, this guide, SHA-256 checksums, and on Windows the
Scheduled Task installer script. They are signed and source-attested by the
same release workflow as the desktop app. The app installer does not install
the relay.

Unzip the archive for your platform and check `release/SHA256SUMS` before
installing. Use the binary with `--post-office`, a persistent `--keystore`, and
a private `--password-file`. Keep the keystore and password together when
updating; replacing either can make queued events inaccessible. The
[Windows](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/post-office-windows-service.md) and
[macOS](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/post-office-macos-service.md) runbooks give service commands.

On Windows, open an elevated PowerShell after extracting the archive, verify
the binary's hash against `release/SHA256SUMS`, then run the bundled installer:

```powershell
cd .\release
foreach ($line in Get-Content .\SHA256SUMS) {
  if ($line -notmatch '^([0-9a-fA-F]{64}) [ *](?:\./)?([^/\\]+)$') { throw 'Invalid checksum manifest' }
  if ((Get-FileHash (Join-Path . $Matches[2]) -Algorithm SHA256).Hash -ne $Matches[1]) { throw "Checksum mismatch: $($Matches[2])" }
}
$binary = (Resolve-Path .\mesh-talk-node.exe).Path
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install-post-office-windows.ps1 -StagedBinary $binary -Name post-office
```

The installer registers a boot-time Scheduled Task, preserves existing relay
data during updates, and checks the TCP listener. On macOS, copy the verified
binary to a persistent path and configure a LaunchAgent as described in the
macOS runbook. The release ZIP is a native binary package, not a desktop app.

## Linux container image

The verified release also publishes a Linux `amd64`/`arm64` image to
`ghcr.io/octopusgarage/mesh-talk-post-office:<tag>` when the draft release is
published. The image contains the same Linux relay binaries as the signed
archives. Its entrypoint enables `--post-office`. It contains no keystore,
relay queue, or password. The tag is
published only after the verified release gate; use the exact version tag in production.

On a **Linux Docker Engine host** on the same LAN, create a persistent private
directory and password file. The container runs as UID/GID 10001 and requires
the password file to have mode `0600`:

```bash
sudo install -d -m 700 -o 10001 -g 10001 /srv/mesh-talk-post-office
sudo sh -c 'openssl rand -base64 32 > /srv/mesh-talk-post-office/password'
sudo chown 10001:10001 /srv/mesh-talk-post-office/password
sudo chmod 600 /srv/mesh-talk-post-office/password
docker run -d --name mesh-talk-post-office --restart unless-stopped \
  --network host \
  --mount type=bind,src=/srv/mesh-talk-post-office,dst=/var/lib/mesh-talk \
  ghcr.io/octopusgarage/mesh-talk-post-office:<tag> \
  --keystore /var/lib/mesh-talk/identity.keystore \
  --password-file /var/lib/mesh-talk/password \
  --name post-office --port 47475 --discovery-port 47474
```

Replace `<tag>` with the release tag, for example `v0.3.2`. Back up the
directory, including its password and keystore, if queued custody matters.
Check startup with `docker logs mesh-talk-post-office` and confirm clients
discover its signed post-office announcement. Allow LAN TCP 47475 and signed
UDP multicast discovery on 47474 through the host firewall. The container
must use `--network host` so the relay can join the LAN multicast group and
advertise an address peers can reach. Docker Desktop's host network has
different network semantics, so use the native relay package on macOS and
Windows. Other relays do not replicate this container's queue.

The owner may need to set the GHCR package visibility to public before
anonymous `docker pull` works. Private package pulls require registry
authentication.
