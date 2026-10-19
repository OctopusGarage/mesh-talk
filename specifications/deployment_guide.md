# Mesh-Talk Deployment Guide

This guide provides instructions for deploying Mesh-Talk on different platforms including macOS, Windows, and Linux. Mesh-Talk ships as a desktop app built with Tauri (the `mesh-talk` binary); a headless node binary (`mesh-talk-node`) is also available for testing and for running a post-office relay.

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Headless Node Deployment](#headless-node-deployment)
3. [GUI Version Deployment](#gui-version-deployment)
4. [Platform-Specific Instructions](#platform-specific-instructions)
   - [macOS](#macos)
   - [Windows](#windows)
   - [Linux](#linux)
5. [Troubleshooting](#troubleshooting)

## Prerequisites

Before deploying Mesh-Talk, ensure you have the following tools installed:

### Rust Toolchain

- Rust compiler and Cargo (latest stable version)
- Installation: https://www.rust-lang.org/tools/install

### Node.js and npm

- Node.js 22 or newer (the version used by CI)
- npm (comes with Node.js)
- Installation: https://nodejs.org/

### Platform-Specific Dependencies

#### macOS

- Xcode Command Line Tools: `xcode-select --install`
- Additional dependencies will be automatically handled by Tauri

#### Windows

- Visual Studio Build Tools or Visual Studio Community
- WebView2 Runtime (usually pre-installed on Windows 10+)

#### Linux

- Development packages for GTK and WebKit
- For Debian/Ubuntu builds: `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf`

## Headless Node Deployment

`mesh-talk-node` is the headless node binary — a CLI over the same core, useful for
testing on a server and for running an always-on **post office** relay.
Tagged releases provide separate signed post-office ZIPs for macOS, Windows,
and Linux. Linux also has an `amd64`/`arm64` GHCR image. See the
[post-office release guide](../docs/post-office-release.md) for packages,
verification, and a persistent Docker Engine setup.

### Building the node

1. Navigate to the project root directory:

   ```bash
   cd mesh-talk
   ```

2. Build the node binary (the bin lives in the `mesh-talk-core` crate):

   ```bash
   cargo build --release -p mesh-talk-core --bin mesh-talk-node
   ```

3. The compiled binary will be located at the workspace root:
   - macOS/Linux: `target/release/mesh-talk-node`
   - Windows: `target/release/mesh-talk-node.exe`

### Running the node

```bash
# a normal headless node
./mesh-talk-node --keystore ~/.mesh-talk/node.keystore --password "<pw>" --name "YourName"
# run as a store-and-forward relay
./mesh-talk-node --keystore ~/.mesh-talk/relay.keystore --password-file ~/.mesh-talk/relay.password --name "relay" --post-office
```

Parameters:

- `--keystore`: path to the encrypted identity keystore (created if absent) — **required**
- `--password` or `--password-file`: password that encrypts the keystore and
  relay log — **one is required**. A service should use a private regular file
  (mode `0600`) so its password is absent from process arguments and the
  launch-agent plist. Store that file in a directory accessible only to its
  owner. Back it up alongside the keystore; losing it makes the stored relay
  identity and ciphertext unreadable.
- `--name`: this node's display name on the LAN — **required**
- `--port`: TCP port for peer connections (default: OS-assigned)
- `--discovery-port`: UDP discovery port (default: 47474; local discovery shares a port, while routed probing can target an explicit remote port)
- `--post-office`: run in relay mode (stores-and-forwards recipient-encrypted
  opaque event wrappers for offline peers)

Desktop users can turn off use of discovered relays in **Settings → Connection & delivery →
Use post-office relays**. The per-device preference is on by default and takes
effect on the running node. Turning it off stops that device's relay uploads and
downloads, so offline delivery may wait until a peer is reachable directly.
Several relays may advertise on one LAN. Clients support a preferred directly
reachable LAN/Tailscale relay with eligible alternatives; an alternative must have
a surviving onward route for cross-site delivery. Independent mailbox-only relays
do not replicate automatically. Equal bridge relays can reconcile eligible opaque
mailboxes after explicit enrollment through a shared public trust manifest. See
[the federation deployment workflow](../docs/post-office-federation.md).

**Settings → Connection & delivery** also exposes independent relay sending/receiving,
public bridge discovery and physical-LAN-only discovery. Master-off overrides relay
data/directory participation without erasing saved preferences. See
[the settings contract](../docs/network-design-reference.md#4-settings-and-deployment-configuration).

The production relay rejects old
clear-header uploads. It cannot read the inner event or channel names, but
its authenticated connections and timing can reveal communication patterns.
Pending events stored only in the older clear-header format require the
original sender to retry through an opaque mailbox or deliver directly.
See [relay metadata privacy](../docs/post-office-metadata-privacy.md).

Discovery uses signed UDP **multicast** (group `224.0.0.167`, port 47474 by default);
peer connections are Noise-encrypted TCP on an OS-assigned (or `--port`) port. The host
firewall must permit UDP 47474 (multicast 224.0.0.167) and the chosen TCP port.

### Installing the node system-wide

#### macOS/Linux

```bash
sudo cp target/release/mesh-talk-node /usr/local/bin/
```

For an Intel Mac, build with the `x86_64-apple-darwin` target and verify the
artifact with `file` before copying it. A macOS LaunchAgent can start the node
at user login with `RunAtLoad` and `KeepAlive`; use an absolute binary path and
pass `--keystore`, `--password-file`, `--name`, `--post-office`, and fixed
`--port`/`--discovery-port` arguments. The agent must run as the same user that
owns the private password file and relay data directory. A LaunchAgent stops
when that user logs out. See the [macOS relay service runbook](../docs/post-office-macos-service.md)
for the deployed `mac2015` paths and health commands.

#### Windows

Copy the `mesh-talk-node.exe` file to a directory in your PATH, or add the target directory to your PATH environment variable.
For an always-on relay, install it as a `SYSTEM` Scheduled Task with a private
password file and LAN-scoped firewall rules. See the
[Windows relay service runbook](../docs/post-office-windows-service.md) for the
tested build, update, health, and cross-machine E2E commands.

## GUI Version Deployment

The GUI version of Mesh-Talk provides a desktop application with a graphical interface.

### Building the GUI Version

1. Navigate to the project root directory:

   ```bash
   cd mesh-talk
   ```

2. Install frontend dependencies:

   ```bash
   cd frontend
   npm install
   cd ..
   ```

3. Build the GUI version for your platform:
   ```bash
   cd src-tauri
   cargo tauri build
   ```

The default edition bundles optional avatar, theme, and sticker assets. For the
lite edition, set `MESH_TALK_VARIANT=lite` before `cargo tauri build`; it omits
optional bundled packs and animated Noto sticker files. Both editions can install
packs later and use the same app identity. See
[`docs/CUSTOMIZATION_PACKS.md`](../docs/CUSTOMIZATION_PACKS.md).

### Build Options

You can customize the build with the following options:

- Build for a specific platform:

  ```bash
  cargo tauri build --target x86_64-apple-darwin    # macOS
  cargo tauri build --target x86_64-pc-windows-msvc # Windows
  cargo tauri build --target x86_64-unknown-linux-gnu # Linux
  ```

- Create a debug build:
  ```bash
  cargo tauri build --debug
  ```

### Installation Packages

After building from `src-tauri/`, installation packages are under the workspace
`target/release/bundle/` (or `target/<triple>/release/bundle/` for an explicit
target). The release workflow packages each target and edition into a ZIP with
checksums and verification artifacts:

- macOS: `.app` bundle and `.dmg` disk image
- Windows: `.exe` (NSIS) and `.msi` installers
- Linux: `.deb`, `.rpm`, and `.AppImage`

## Platform-Specific Instructions

### macOS

#### Prerequisites

- Xcode Command Line Tools

#### Installation

1. Download the `.dmg` file or use the `.app` bundle
2. Drag the Mesh-Talk application to your Applications folder
3. Launch from Applications or Spotlight search

#### Code Signing (for distribution)

For distribution outside the App Store, you may need to code sign the application:

```bash
# This requires an Apple Developer account
codesign --force --deep --sign "Developer ID Application: YOUR_NAME" /path/to/mesh-talk.app
```

### Windows

#### Prerequisites

- Windows 10 or higher
- WebView2 Runtime (usually pre-installed)

#### Installation

1. Download the `.msi` installer
2. Run the installer and follow the installation wizard
3. Launch from the Start menu

### Linux

#### Prerequisites

- A distribution with the Tauri 2 WebKit/GTK runtime dependencies

#### Installation Options

##### Using .deb Package (Debian/Ubuntu)

```bash
sudo dpkg -i mesh-talk_x.x.x_amd64.deb
sudo apt-get install -f  # Fix any dependency issues
```

##### Using AppImage

1. Download the `.AppImage` file
2. Make it executable:
   ```bash
   chmod +x mesh-talk_x.x.x_amd64.AppImage
   ```
3. Run directly or move to `/usr/local/bin/` for system-wide access

#### System Integration

The `.deb` package adds a desktop launcher; inspect the package contents if you
need its exact installed paths on your distribution.

## Troubleshooting

### Common Build Issues

#### Missing Dependencies

If you encounter missing dependencies during build:

```bash
# macOS
xcode-select --install

# Ubuntu/Debian
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf

# For other distributions, use the Tauri 2 prerequisites for your target.
```

#### Rust Compilation Errors

Update your Rust toolchain:

```bash
rustup update
```

#### Node.js Version Issues

Ensure you're using a compatible Node.js version:

```bash
node --version  # CI uses Node.js 22
```

### Runtime Issues

#### Application Won't Start

1. Check that all prerequisites are installed
2. Verify the application has necessary permissions
3. Check system logs for error messages

#### Network Discovery Not Working

1. Ensure UDP port 47474 (multicast group 224.0.0.167) is not blocked by firewall
2. For automatic LAN discovery, check that nodes share a local network segment;
   for routed or bridged discovery, check the configured network path and client
   opt-in in the [topology guide](../docs/network-topologies.md)
3. Verify that the network allows multicast/UDP between hosts (some guest/AP-isolated
   Wi-Fi blocks it); on macOS the app ships the `com.apple.developer.networking.multicast`
   entitlement so the OS permits multicast

#### Local Storage Issues

Mesh-Talk keeps protocol and account data as encrypted files under `~/.mesh-talk`.
Optional installed packs are kept separately in the webview's IndexedDB, and some
UI preferences use local storage. If an encrypted account store appears corrupt:

1. Close the application and preserve a copy of the affected data directory before
   attempting recovery. Keep the original encrypted stores and keys intact.
2. Record the error and check available disk space, file permissions and whether
   another process is using the profile. Do not remove an account directory as a
   general repair step.
3. If you have a verified `.mtbackup` archive and its matching recovery key, follow
   [backup and recovery](../docs/backup-recovery.md) to restore under an unused local
   username. Restore creates a new device; it does not repair or merge into the
   damaged profile or revoke its identity.

Peer synchronization can supply only retained, authorized content over an available
path. It cannot replace missing local keys or guarantee complete history or file
bytes. Keep the preserved profile for diagnosis if no verified recovery path is
available.

### Platform-Specific Troubleshooting

#### macOS

- If Gatekeeper prevents the app from opening:
  - Right-click the app and select "Open"
  - See the first-run instructions in the root README for an unsigned build

#### Windows

- If WebView2 is missing:
  - Download and install from https://developer.microsoft.com/en-us/microsoft-edge/webview2/
- If SmartScreen blocks the installer:
  - Click "More info" and then "Run anyway"

#### Linux

- If the AppImage won't run:
  - Install FUSE: `sudo apt install fuse`
  - Or run with: `./mesh-talk.AppImage --no-sandbox`

## Support

For additional help, please:

1. Check the project documentation
2. Search existing issues on the project's GitHub repository
3. Create a new issue with detailed information about your problem

For current release packages and verification steps, use the
[Releases page](https://github.com/OctopusGarage/mesh-talk/releases) and the
[release guide](../docs/updates.md).
