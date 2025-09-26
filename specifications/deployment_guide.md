# Mesh-Talk Deployment Guide

This guide provides instructions for deploying Mesh-Talk on different platforms including macOS, Windows, and Linux. Mesh-Talk can be deployed in two versions: a Command Line Interface (CLI) version and a Graphical User Interface (GUI) version built with Tauri.

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [CLI Version Deployment](#cli-version-deployment)
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
- Node.js (version 16 or higher)
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
- For Debian/Ubuntu: `sudo apt install libwebkit2gtk-4.0-dev build-essential curl wget libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`

## CLI Version Deployment

The CLI version of Mesh-Talk provides a command-line interface for local network chatting.

### Building the CLI Version

1. Navigate to the project root directory:
   ```bash
   cd mesh-talk
   ```

2. Build the CLI version:
   ```bash
   cd src-tauri
   cargo build --release
   ```

3. The compiled binary will be located at:
   - macOS/Linux: `target/release/mesh-talk`
   - Windows: `target/release/mesh-talk.exe`

### Running the CLI Version

Execute the binary with the required parameters:
```bash
./mesh-talk --name "YourName" --port 7000
```

Parameters:
- `--name` or `-n`: Your display name in the chat
- `--port` or `-p`: TCP port to listen on (default: 7000)

### Installing the CLI Version System-Wide

To make the CLI version available system-wide:

#### macOS/Linux
```bash
sudo cp target/release/mesh-talk /usr/local/bin/
```

#### Windows
Copy the `mesh-talk.exe` file to a directory in your PATH, or add the target directory to your PATH environment variable.

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

After building, the installation packages will be located in `src-tauri/target/release/bundle/`:

- macOS: `.app` bundle and `.dmg` disk image
- Windows: `.msi` installer
- Linux: `.deb` package (Debian/Ubuntu) and `.AppImage`

## Platform-Specific Instructions

### macOS

#### Prerequisites
- macOS 10.15 or higher
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

#### System Requirements
- Minimum: 2 GB RAM, 100 MB disk space
- Recommended: 4 GB RAM, 200 MB disk space

### Linux

#### Prerequisites
- Most modern Linux distributions (Ubuntu 20.04+, Debian 11+, Fedora 34+)
- Required system libraries (automatically installed with .deb package)

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
The .deb package will automatically:
- Install the application to `/opt/mesh-talk/`
- Create a desktop entry
- Add the application to the system menu

## Troubleshooting

### Common Build Issues

#### Missing Dependencies
If you encounter missing dependencies during build:
```bash
# macOS
xcode-select --install

# Ubuntu/Debian
sudo apt update
sudo apt install libwebkit2gtk-4.0-dev build-essential curl wget libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev

# Fedora
sudo dnf install webkit2gtk4.0-devel openssl-devel clang
```

#### Rust Compilation Errors
Update your Rust toolchain:
```bash
rustup update
```

#### Node.js Version Issues
Ensure you're using a compatible Node.js version:
```bash
node --version  # Should be 16 or higher
```

### Runtime Issues

#### Application Won't Start
1. Check that all prerequisites are installed
2. Verify the application has necessary permissions
3. Check system logs for error messages

#### Network Discovery Not Working
1. Ensure UDP port 9999 is not blocked by firewall
2. Check that all nodes are on the same local network
3. Verify that broadcast is enabled on your network

#### Database Issues
Mesh-Talk uses SQLite for local storage. If you encounter database issues:
1. Close the application
2. Backup and remove the database file (usually in the application data directory)
3. Restart the application to create a new database

### Platform-Specific Troubleshooting

#### macOS
- If Gatekeeper prevents the app from opening:
  - Right-click the app and select "Open"
  - Or temporarily disable Gatekeeper: `sudo spctl --master-disable`

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

Version: 0.1.0
Last Updated: 2025-09-19