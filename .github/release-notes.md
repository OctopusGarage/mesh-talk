## Mesh-Talk __TAG__

Serverless, end-to-end-encrypted LAN messenger (Rust + Tauri 2 + React). No server and no
sign-up — peers on the same local network discover each other over signed broadcasts and chat
directly with forward secrecy. Includes 1:1 DMs, group channels, file sharing, reactions,
replies, @mentions, search, and multi-device.

### What's new in v0.3.0

- Install and remove avatar, theme, and sticker ZIP libraries, including your own packs.
- Choose the default edition with 14 optional packs preinstalled or the lite edition with
  no optional packs. Both editions keep light, dark, OLED, and built-in Noto stickers.
- Browse and install shared packs from the [OctopusGarage Marketplace](https://octopusgarage.github.io/marketplace/),
  with versioned downloads and SHA-256 verification. The [Pack Studio](https://octopusgarage.github.io/marketplace/studio/)
  builds and checks packs locally in your browser.
- Track each file transfer and open downloads when they are ready.
- Attach directories with safe extraction and a shortcut to the downloads folder.
- Capture and annotate screenshots on macOS, Windows, and Linux, subject to desktop capture
  permissions and portal support.

### Downloads

Pick the zip for your platform. Each zip contains the installer(s) **plus** a `SHA256SUMS`
file, and ships alongside a cosign signature (`.zip.bundle`) and a SLSA build-provenance
attestation. A CycloneDX SBOM (`mesh-talk.cdx.json`) is attached to the release.

Each platform has a default edition with the current avatar and personal theme collections preinstalled, and a lite edition with an empty optional library. Both editions retain light, dark, and OLED themes and support ZIP installation. Add `_lite` before `.zip` in the table below to select the lite edition. The two editions replace one another on the same device and preserve packs you have installed or removed.

| Platform | Asset |
|----------|-------|
| macOS (Apple Silicon) | `mesh-talk___TAG___macos_arm64.zip` |
| macOS (Intel) | `mesh-talk___TAG___macos_x86_64.zip` |
| Windows (x64) | `mesh-talk___TAG___windows_x86_64.zip` |
| Windows (ARM64) | `mesh-talk___TAG___windows_arm64.zip` |
| Linux (x64) | `mesh-talk___TAG___linux_x86_64.zip` |
| Linux (ARM64) | `mesh-talk___TAG___linux_aarch64.zip` |

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
mesh-talk-node --post-office
```

The post office only ever stores **ciphertext** — it cannot read messages.

Full documentation: [README](https://github.com/OctopusGarage/mesh-talk#readme) ·
[ARCHITECTURE](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/ARCHITECTURE.md) ·
[SECURITY](https://github.com/OctopusGarage/mesh-talk/blob/main/SECURITY.md)
