## Mesh-Talk __TAG__

End-to-end-encrypted LAN messenger (Rust + Tauri 2 + React). No central server or
sign-up — peers on the same local network discover each other over signed broadcasts and chat
directly with forward secrecy. Includes 1:1 DMs, group channels, file sharing, reactions,
replies, @mentions, search, and multi-device.

### What's new in v0.3.2

- Recover missed channel messages from available group members after reconnecting, even when
  the original sender has gone offline.
- Share encrypted channel history and retained attachments among group members by default,
  with a visible per-member option to disable replica sharing.
- Bound sender fanout for larger channels and retry delivery until enough member copies are
  confirmed. Small channels continue direct delivery.
- Restrict automatic direct-message catch-up to contacts whose safety number you manually
  verified. Incoming direct messages can still arrive by direct push.
- Improve offline recovery and delivery status while keeping file transfers separate from
  message propagation.
- Add pairwise opaque post-office mailboxes, a per-account relay preference, separate
  native relay packages, and a Linux container image published with the verified release.


### Downloads

Pick the zip for your platform. Each zip contains the installer(s) **plus** a `SHA256SUMS`
file, and ships alongside a cosign signature (`.zip.bundle`) and a SLSA build-provenance
attestation. A CycloneDX SBOM (`mesh-talk.cdx.json`) is attached to the release.
The macOS zip contains a DMG; the app is inside that DMG.

Each platform has a default edition with the current avatar and personal theme collections preinstalled, and a lite edition with an empty optional library. Both editions retain light, dark, and OLED themes and support ZIP installation. Lite shows emoji fallbacks for built-in Noto stickers without bundling their animations. Add `_lite` before `.zip` in the table below to select the lite edition. The two editions replace one another on the same device and preserve packs you have installed or removed.

| Platform | Asset |
|----------|-------|
| macOS (Apple Silicon) | `mesh-talk___TAG___macos_arm64.zip` |
| macOS (Intel) | `mesh-talk___TAG___macos_x86_64.zip` |
| Windows (x64) | `mesh-talk___TAG___windows_x86_64.zip` |
| Windows (ARM64) | `mesh-talk___TAG___windows_arm64.zip` |
| Linux (x64) | `mesh-talk___TAG___linux_x86_64.zip` |
| Linux (ARM64) | `mesh-talk___TAG___linux_aarch64.zip` |

### Post-office relay downloads

An always-on relay has its own headless package, separate from the desktop app. Choose
`mesh-talk-post-office___TAG___<os>_<arch>.zip` using the same platform names above.
Each relay ZIP contains `mesh-talk-node`, a setup guide and `SHA256SUMS` (plus a
Windows service installer script on Windows). Its `.zip.bundle` signature and
build provenance are verified by the release workflow along with the app packages.

For Linux Docker Engine, the same verified Linux binaries are also published as
`ghcr.io/octopusgarage/mesh-talk-post-office:__TAG__` for `linux/amd64` and
`linux/arm64`. Run it with host networking, a persistent data directory, and
an owner-only password file. See the [post-office package and container guide](https://github.com/OctopusGarage/mesh-talk/blob/__TAG__/docs/post-office-release.md).

### Verifying your download

These builds are **not signed by a commercial CA** (free distribution), but every artifact is
independently verifiable. Replace `<os>_<arch>` with your platform (e.g. `macos_arm64`).
For lite, append `_lite` after `<os>_<arch>` in the examples.

**1. Checksum** — unzip, then in the extracted folder:
- macOS / Linux: `shasum -a 256 -c SHA256SUMS`
- Windows (PowerShell): `Get-FileHash .\<file> -Algorithm SHA256` and compare to `SHA256SUMS`

**2. Cosign signature** (Sigstore keyless — proves the zip is the one CI produced):
```sh
cosign verify-blob \
  --bundle mesh-talk___TAG___<os>_<arch>.zip.bundle \
  --certificate-identity-regexp 'https://github.com/OctopusGarage/mesh-talk/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  mesh-talk___TAG___<os>_<arch>.zip
```

**3. SLSA build provenance** (proves it was built by this repo's CI from tag `__TAG__`):
```sh
gh attestation verify mesh-talk___TAG___<os>_<arch>.zip --repo OctopusGarage/mesh-talk
```

**4. SBOM** — `mesh-talk.cdx.json` is the full CycloneDX dependency inventory for auditing.

### First run (unsigned app)

- **macOS**: right-click the app → **Open** (first time only). If it's blocked as "damaged",
  clear the quarantine flag. Use the **full path** `/usr/bin/xattr` — a Homebrew or Conda `xattr`
  earlier in your `PATH` can shadow the system one and won't accept `-r`:
  `/usr/bin/xattr -dr com.apple.quarantine /Applications/mesh-talk.app`
- **Windows**: SmartScreen → **More info** → **Run anyway**.
- **Linux**: `chmod +x *.AppImage` and run it, or install the `.deb` / `.rpm`.

### Using it

Launch the app and register a username + password — your keys are generated and stored
encrypted on-device (PBKDF2 + AES-GCM); there is no central account. Other people running
Mesh-Talk on the **same LAN** appear automatically; click a peer to start chatting.

For offline delivery (so peers still receive messages when you're not both online), run the
headless relay on an always-on machine:

```sh
mesh-talk-node --keystore ./relay.keystore --password-file ./relay.password \
  --name post-office --post-office --port 47475
```

The post office stores encrypted event wrappers. It needs routing metadata to
deliver them, and connection timing can reveal communication patterns. See the
[relay metadata privacy guide](https://github.com/OctopusGarage/mesh-talk/blob/__TAG__/docs/post-office-metadata-privacy.md).

Full documentation: [README](https://github.com/OctopusGarage/mesh-talk#readme) ·
[ARCHITECTURE](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/ARCHITECTURE.md) ·
[SECURITY](https://github.com/OctopusGarage/mesh-talk/blob/main/SECURITY.md)
