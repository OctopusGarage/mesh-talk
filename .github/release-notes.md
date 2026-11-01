## Mesh-Talk `__TAG__`

End-to-end-encrypted LAN messenger (Rust + Tauri 2 + React). No central server or
sign-up — peers on the same local network discover each other over signed broadcasts and chat
directly with forward secrecy. Includes 1:1 DMs, group channels, file sharing, reactions,
replies, @mentions, search, and multi-device.

### Downloads

**Choose one download for your computer.** Each ZIP contains the installers you need.
On Mac, choose Apple Silicon for M-series chips or Intel for older Intel-based Macs.

| System | Processor | Download |
| --- | --- | --- |
| macOS | Apple Silicon (M-series) | [Download ZIP](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk___TAG___macos_arm64.zip) |
| macOS | Intel | [Download ZIP](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk___TAG___macos_x86_64.zip) |
| Windows | x64 (Intel / AMD) | [Download ZIP](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk___TAG___windows_x86_64.zip) |
| Windows | ARM64 | [Download ZIP](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk___TAG___windows_arm64.zip) |
| Linux | x64 (Intel / AMD) | [Download ZIP](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk___TAG___linux_x86_64.zip) |
| Linux | ARM64 | [Download ZIP](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk___TAG___linux_aarch64.zip) |

Each platform has one desktop package. The app starts without optional avatar, theme, or sticker packs and retains light, dark, and OLED themes. Install packs from the marketplace or a local ZIP. Built-in Noto stickers show emoji fallbacks until their artwork pack is installed. Upgrading preserves packs already installed on the device.

__RELEASE_HIGHLIGHTS__

### Installation

Quit Mesh-Talk before installing an update. Your existing local account and data
remain in `~/.mesh-talk`; the update replaces the existing application.

- **macOS**: unzip the download, open the `.dmg`, then drag `mesh-talk.app` into
  **Applications**. Replace the existing app when upgrading, then launch it from
  Applications. See **First run** below if macOS blocks the unsigned app.
- **Windows**: unzip the download and run the `.exe` installer. The included `.msi`
  is an alternative for managed installations. Launch Mesh-Talk after installation.
- **Linux**: unzip the download, then install the `.deb` or `.rpm` with your package
  manager. Alternatively, make the `.AppImage` executable with `chmod +x *.AppImage`
  and run it.

### Post-office relay downloads

<details>
<summary>Headless relay packages for always-on machines</summary>

Use these packages to host a relay. For the desktop app, use the downloads above.

| System | Processor | Download |
| --- | --- | --- |
| macOS | Apple Silicon | [Download relay](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk-post-office___TAG___macos_arm64.zip) |
| macOS | Intel | [Download relay](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk-post-office___TAG___macos_x86_64.zip) |
| Windows | x64 | [Download relay](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk-post-office___TAG___windows_x86_64.zip) |
| Windows | ARM64 | [Download relay](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk-post-office___TAG___windows_arm64.zip) |
| Linux | x64 | [Download relay](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk-post-office___TAG___linux_x86_64.zip) |
| Linux | ARM64 | [Download relay](https://github.com/__REPOSITORY__/releases/download/__TAG__/mesh-talk-post-office___TAG___linux_aarch64.zip) |

Each relay ZIP contains `mesh-talk-node`, a setup guide and `SHA256SUMS` (plus a
Windows service installer script on Windows). Its `.zip.bundle` signature and
build provenance are verified by the release workflow along with the app packages.

<!-- STABLE_ONLY_START -->
For Linux Docker Engine, the same verified Linux binaries are also published as
`__CONTAINER_IMAGE__:__TAG__` for `linux/amd64` and
`linux/arm64`. Run it with host networking, a persistent data directory, and
an owner-only password file. See the [post-office package and container guide](https://github.com/__REPOSITORY__/blob/__TAG__/docs/post-office-release.md).
<!-- STABLE_ONLY_END -->

</details>

<details>
<summary>Verifying your download</summary>

The additional update, patch and signature assets serve automatic updates and verification.
You do not need to select them to install the app. Each desktop ZIP includes installer
`SHA256SUMS` and, when updates are enabled, a small `UPDATER_SHA256SUMS` manifest binding
the separately published update assets. The ZIP's `.bundle` signature and build provenance
authenticate both manifests; update payloads are not duplicated in the installer ZIP.

These builds are **not signed by a commercial CA** (free distribution), but every artifact is
independently verifiable. Replace `<os>_<arch>` with your platform (e.g. `macos_arm64`).

**1. Checksum** — unzip, then in the extracted folder:

- macOS / Linux: `shasum -a 256 -c SHA256SUMS`
- Windows (PowerShell): `Get-FileHash .\<file> -Algorithm SHA256` and compare to `SHA256SUMS`

**2. Cosign signature** (Sigstore keyless — proves the zip is the one CI produced):

```sh
cosign verify-blob \
  --bundle mesh-talk___TAG___<os>_<arch>.zip.bundle \
  --certificate-identity 'https://github.com/__REPOSITORY__/.github/workflows/release.yml@__SOURCE_REF__' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  mesh-talk___TAG___<os>_<arch>.zip
```

**3. SLSA build provenance** (binds the archive to this workflow, source ref `__SOURCE_REF__` and exact commit `__SOURCE_SHA__`):

```sh
gh attestation verify mesh-talk___TAG___<os>_<arch>.zip --repo '__REPOSITORY__' \
  --source-ref '__SOURCE_REF__' --source-digest '__SOURCE_SHA__' \
  --signer-workflow '__REPOSITORY__/.github/workflows/release.yml' \
  --deny-self-hosted-runners
```

**4. SBOM** — `mesh-talk.cdx.json` is the full CycloneDX dependency inventory for auditing.

</details>

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
[relay metadata privacy guide](https://github.com/__REPOSITORY__/blob/__TAG__/docs/post-office-metadata-privacy.md).

Full documentation: [README](https://github.com/__REPOSITORY__#readme) ·
[ARCHITECTURE](https://github.com/__REPOSITORY__/blob/main/docs/ARCHITECTURE.md) ·
[SECURITY](https://github.com/__REPOSITORY__/blob/main/SECURITY.md)
