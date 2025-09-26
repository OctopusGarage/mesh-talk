# Platform-Specific Permissions and Configurations

This document describes the platform-specific permissions and configurations implemented for Mesh-Talk to ensure proper operation across different operating systems.

## Windows

### Permissions and Configurations

1. **Code Signing**: The Windows configuration includes settings for code signing with a certificate thumbprint. This is essential for distributing trusted applications on Windows.

2. **Digest Algorithm**: SHA-256 is specified as the digest algorithm for code signing, which is the current industry standard.

3. **Timestamp Server**: A timestamp URL can be configured to ensure the signature remains valid even after the certificate expires.

4. **Webview Installation Mode**: The application is configured to download the WebView2 bootstrapper, which is the recommended approach for distributing Tauri applications on Windows.

### Secure Storage

Windows applications can use the Data Protection API (DPAPI) for secure storage of sensitive information. The current implementation is a placeholder that will be enhanced to use the actual DPAPI.

### Notifications

Windows notifications are supported through the Windows Notification API. The current implementation is a placeholder that will be enhanced to use the actual Windows notification system.

## Linux

### Permissions and Configurations

1. **Debian Package Dependencies**: The Linux configuration includes a section for specifying Debian package dependencies. Currently empty but can be extended as needed.

### Secure Storage

Linux applications can use the Secret Service API for secure storage of sensitive information. The current implementation is a placeholder that will be enhanced to use the actual Secret Service API.

### Notifications

Linux notifications are supported through D-Bus. The current implementation is a placeholder that will be enhanced to use the actual D-Bus notification system.

## macOS

### Permissions and Configurations

1. **Entitlements**: The entitlements.plist file specifies the permissions required by the application, including:
   - App Sandbox
   - Network client access
   - Network server access
   - File system access for user-selected files

2. **Code Signing**: Configuration for code signing with a developer ID and provider short name.

3. **Minimum System Version**: Specifies the minimum macOS version required to run the application.

### Secure Storage

macOS applications can use the Keychain Services for secure storage of sensitive information. The current implementation uses the keyring crate to interact with the macOS Keychain.

### Notifications

macOS notifications are supported through the Notification Center. The current implementation uses the notify-rust crate to interact with the macOS notification system.