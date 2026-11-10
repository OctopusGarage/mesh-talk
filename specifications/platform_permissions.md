# Platform-Specific Permissions and Configurations

This document describes the platform-specific permissions and configurations implemented for Mesh-Talk to ensure proper operation across different operating systems.

> **Networking note (all platforms):** Mesh-Talk discovers peers via signed UDP
> **multicast** (group `224.0.0.167`, port `47474` by default) and connects over
> Noise-encrypted TCP on an OS-assigned port. The host firewall must permit UDP 47474
> and the chosen TCP port.
>
> **At-rest secrets (all platforms):** identity/account keystores and protocol stores
> are encrypted with PBKDF2-HMAC-SHA256 (600k rounds) + AES-256-GCM under the login
> password and held as files under `~/.mesh-talk/`. If **Stay signed in** is enabled,
> the resumable login credential is saved separately in the OS credential store.
> On macOS, reading it requires Touch ID on supported systems.

## Windows

### Permissions and Configurations

1. **Code Signing**: The Windows configuration includes settings for code signing with a certificate thumbprint. This is essential for distributing trusted applications on Windows.

2. **Digest Algorithm**: SHA-256 is specified as the digest algorithm for code signing, which is the current industry standard.

3. **Timestamp Server**: A timestamp URL can be configured to ensure the signature remains valid even after the certificate expires.

4. **Webview Installation Mode**: The application is configured to download the WebView2 bootstrapper, which is the recommended approach for distributing Tauri applications on Windows.

### Secure Storage

Protocol secrets are stored as PBKDF2 + AES-256-GCM encrypted files (see the
at-rest note above). Optional saved-session credentials use Windows Credential Manager.

### Notifications

Windows notifications are delivered through `tauri-plugin-notification` (the native
Windows toast system).

## Linux

### Permissions and Configurations

1. **Debian Package Dependencies**: The Linux configuration includes a section for specifying Debian package dependencies. Currently empty but can be extended as needed.

### Screenshots

Mesh-Talk prefers the desktop Screenshot portal. X11 desktops without that
capability can use the optional `xfce4-screenshooter` executable (Ubuntu package
of the same name). The fallback is enabled only for an actual X11 app window;
an XWayland display variable alone does not enable it for native Wayland apps.
Portal cancellation and capture errors never invoke a fallback. The helper uses
a private temporary PNG, a 15-second execution limit and a 128 MiB image limit;
temporary output is removed on success or failure. See the
[screenshot guide](../docs/help/topics/screenshots.md#platform-behavior).

### Secure Storage

Protocol secrets are stored as PBKDF2 + AES-256-GCM encrypted files (see the
at-rest note above). Optional saved-session credentials use the Secret Service API.

### Notifications

Linux notifications are delivered through `tauri-plugin-notification` (D-Bus desktop
notifications).

## macOS

### Permissions and Configurations

1. **Entitlements**: `src-tauri/entitlements.plist` specifies the permissions required by
   the application:
   - App Sandbox (`com.apple.security.app-sandbox`)
   - Network client access (`com.apple.security.network.client`)
   - Network server access (`com.apple.security.network.server`)
   - **Multicast networking** (`com.apple.developer.networking.multicast`) — required so
     the sandbox permits the UDP multicast discovery (group `224.0.0.167`)
   - File-system access for user-selected files
     (`com.apple.security.files.user-selected.read-write`) — used by file send/save

2. **Code Signing**: Configuration for code signing with a developer ID and provider short name.

3. **Minimum System Version**: Specifies the minimum macOS version required to run the application.

### Secure Storage

Protocol secrets are stored as PBKDF2 + AES-256-GCM encrypted files (see the
at-rest note above). Optional saved-session credentials use a device-local Keychain
item protected by the current Touch ID enrollment on supported macOS versions.

### Notifications

macOS notifications are delivered through `tauri-plugin-notification` (Notification Center).
