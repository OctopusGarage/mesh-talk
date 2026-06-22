# Platform-Specific Permissions and Configurations

This document describes the platform-specific permissions and configurations implemented for Mesh-Talk to ensure proper operation across different operating systems.

> **Networking note (all platforms):** Mesh-Talk discovers peers via signed UDP
> **multicast** (group `224.0.0.167`, port `47474` by default) and connects over
> Noise-encrypted TCP on an OS-assigned port. The host firewall must permit UDP 47474
> and the chosen TCP port.
>
> **At-rest secrets (all platforms):** identity/account keystores and every store are
> encrypted with PBKDF2-HMAC-SHA256 (600k rounds) + AES-256-GCM under the login password
> and held as files under `~/.mesh-talk/` — Mesh-Talk does **not** use OS keychains.

## Windows

### Permissions and Configurations

1. **Code Signing**: The Windows configuration includes settings for code signing with a certificate thumbprint. This is essential for distributing trusted applications on Windows.

2. **Digest Algorithm**: SHA-256 is specified as the digest algorithm for code signing, which is the current industry standard.

3. **Timestamp Server**: A timestamp URL can be configured to ensure the signature remains valid even after the certificate expires.

4. **Webview Installation Mode**: The application is configured to download the WebView2 bootstrapper, which is the recommended approach for distributing Tauri applications on Windows.

### Secure Storage

Secrets are stored as PBKDF2 + AES-256-GCM encrypted files (see the at-rest note above),
not via DPAPI.

### Notifications

Windows notifications are delivered through `tauri-plugin-notification` (the native
Windows toast system).

## Linux

### Permissions and Configurations

1. **Debian Package Dependencies**: The Linux configuration includes a section for specifying Debian package dependencies. Currently empty but can be extended as needed.

### Secure Storage

Secrets are stored as PBKDF2 + AES-256-GCM encrypted files (see the at-rest note above),
not via the Secret Service API.

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

Secrets are stored as PBKDF2 + AES-256-GCM encrypted files (see the at-rest note above),
not via Keychain Services.

### Notifications

macOS notifications are delivered through `tauri-plugin-notification` (Notification Center).