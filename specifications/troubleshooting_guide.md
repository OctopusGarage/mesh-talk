# Mesh-Talk Troubleshooting Guide

This guide helps you diagnose and resolve common issues with Mesh-Talk. If you're experiencing problems with the application, this guide will help you identify and fix the most common issues.

## Table of Contents

1. [Network Discovery Issues](#network-discovery-issues)
2. [Port and Connection Issues](#port-and-connection-issues)
3. [Message Delivery Issues](#message-delivery-issues)
4. [Platform-Specific Issues](#platform-specific-issues)
5. [Build and Installation Issues](#build-and-installation-issues)
6. [Performance Issues](#performance-issues)
7. [Advanced Troubleshooting](#advanced-troubleshooting)
8. [Getting Help](#getting-help)

## Network Discovery Issues

### Peers Not Being Discovered

**Symptoms:**
- You can't see other users on the network
- Other users can't see you
- No peers appear in the contact list

**Possible Causes and Solutions:**

1. **Firewall Blocking UDP Traffic**
   - Mesh-Talk discovers peers via signed UDP **multicast** (group `224.0.0.167`,
     port `47474` by default) plus a unicast /24 scan fallback
   - Ensure your firewall allows UDP traffic on port 47474 (and the OS-assigned TCP port)
   - On macOS:
     ```bash
     sudo pfctl -f /etc/pf.conf
     ```
   - On Ubuntu/Debian:
     ```bash
     sudo ufw allow 47474/udp
     ```
   - On Windows:
     - Open Windows Defender Firewall
     - Allow an app through the firewall
     - Add Mesh-Talk or allow port 47474 UDP

2. **Network Segmentation**
   - Mesh-Talk only works within the same local network segment
   - Routers and switches may block multicast traffic
   - Verify all users are on the same subnet (e.g., 192.168.1.x)

3. **Multicast Disabled**
   - Some networks (guest / AP-isolated Wi-Fi) block multicast and inter-host UDP
   - Contact your network administrator to enable multicast, or use a different network
   - On macOS the app ships the `com.apple.developer.networking.multicast` entitlement
     so the OS permits multicast — no extra user action is needed

4. **Multiple Network Interfaces**
   - Mesh-Talk joins the multicast group on every non-loopback IPv4 interface and
     periodically re-joins as interfaces change — there is no interface flag to set
   - If discovery is flaky, ensure the intended interface is up and on the same subnet
     as the peers

### Partial Discovery

**Symptoms:**
- Some peers are discovered, but not all
- Inconsistent peer visibility

**Solutions:**
1. **Timing Issues**
   - Mesh-Talk re-announces over multicast every 2 seconds (with a startup burst)
   - Wait ~10-15 seconds for all peers to appear
   - Restart the application if peers still don't appear

2. **Network Load**
   - High network traffic may delay discovery packets
   - Try during periods of lower network usage

## Port and Connection Issues

### TCP Port Conflicts

**Symptoms:**
- Error message: "Failed to bind to port"
- Error message: "Address already in use"
- Application fails to start

**Solutions:**
1. **Use a Different Port** (headless node)
   ```bash
   ./mesh-talk-node --keystore ~/.mesh-talk/node.keystore --password "<pw>" \
     --name "YourName" --port 7001
   ```

2. **Find and Kill the Process Using the Port**
   - On macOS/Linux:
     ```bash
     lsof -i :7000
     kill -9 <PID>
     ```
   - On Windows:
     ```cmd
     netstat -ano | findstr :7000
     taskkill /PID <PID> /F
     ```

3. **Enable Dynamic Port Allocation**
   - Mesh-Talk automatically tries dynamic allocation if the requested port is in use
   - The application will display the actual port being used

### Connection Refused Errors

**Symptoms:**
- Error message: "Connection refused"
- Unable to send messages to specific peers
- Peers appear offline

**Solutions:**
1. **Verify Peer Status**
   - Ensure the peer application is running
   - Check if the peer is on the same network

2. **Firewall Settings**
   - Mesh-Talk uses dynamic TCP ports for communication
   - Configure your firewall to allow connections on a range of ports
   - Example for Ubuntu/Debian:
     ```bash
     sudo ufw allow 7000:8000/tcp
     ```

3. **Network Connectivity**
   - Test basic connectivity:
     ```bash
     ping <peer_ip_address>
     telnet <peer_ip_address> <peer_port>
     ```

## Message Delivery Issues

### Messages Not Sending

**Symptoms:**
- Messages appear to send but don't reach recipients
- Error messages about failed delivery
- Messages stay in "sending" status

**Solutions:**
1. **Check Peer Connections**
   - Verify that peers are online and connected
   - Restart the application to reestablish connections

2. **Network Issues**
   - Check for network connectivity problems
   - Ensure no firewall is blocking TCP traffic

3. **Application Restart**
   - Restart Mesh-Talk on both sender and receiver
   - Clear any cached connection data

### Messages Not Receiving

**Symptoms:**
- No incoming messages despite peers being online
- Delayed message delivery
- Messages arrive out of order

**Solutions:**
1. **Verify Listening Port**
   - Ensure the application is listening on the correct port
   - Check the startup messages for port information

2. **Network Configuration**
   - Verify that TCP connections can be established
   - Check router and firewall settings

3. **Application Logs**
   - Run the headless node with debug logging to see detailed information:
     ```bash
     RUST_LOG=debug ./mesh-talk-node --keystore ~/.mesh-talk/node.keystore \
       --password "<pw>" --name "YourName"
     ```

## Platform-Specific Issues

### macOS Issues

**Symptoms:**
- Application blocked by Gatekeeper
- Permission denied errors
- Network discovery not working

**Solutions:**
1. **Gatekeeper Bypass**
   - Right-click the application and select "Open"
   - Or disable Gatekeeper temporarily:
     ```bash
     sudo spctl --master-disable
     ```

2. **Network Permissions**
   - Ensure the application has network access in System Preferences
   - Go to Security & Privacy > Privacy > Network

3. **Firewall Settings**
   - Add Mesh-Talk to the firewall exceptions
   - System Preferences > Security & Privacy > Firewall > Firewall Options

### Windows Issues

**Symptoms:**
- SmartScreen warnings
- Missing WebView2 Runtime
- Network discovery problems

**Solutions:**
1. **WebView2 Installation**
   - Download and install WebView2 from Microsoft's website
   - Or use the installer that includes WebView2

2. **SmartScreen Bypass**
   - Click "More info" and then "Run anyway"
   - Add the application to Windows Defender exclusions

3. **Firewall Configuration**
   - Allow Mesh-Talk through Windows Firewall
   - Add UDP port 47474 (multicast 224.0.0.167) and the OS-assigned TCP port to exceptions

### Linux Issues

**Symptoms:**
- Missing dependencies
- AppImage not running
- Desktop integration problems

**Solutions:**
1. **Install Dependencies**
   - For Debian/Ubuntu:
     ```bash
     sudo apt install libwebkit2gtk-4.0-dev libayatana-appindicator3-dev
     ```
   - For Fedora:
     ```bash
     sudo dnf install webkit2gtk4.0-devel libappindicator-gtk3
     ```

2. **AppImage Permissions**
   - Make the AppImage executable:
     ```bash
     chmod +x mesh-talk.AppImage
     ```
   - Install FUSE if needed:
     ```bash
     sudo apt install fuse
     ```

3. **Desktop Integration**
   - Install the application using the .deb package for better integration
   - Or manually create a desktop entry

## Build and Installation Issues

### Rust Compilation Errors

**Symptoms:**
- Compilation fails with error messages
- Missing dependencies
- Version conflicts

**Solutions:**
1. **Update Rust Toolchain**
   ```bash
   rustup update
   ```

2. **Install Missing Dependencies**
   - Check the error message for missing system libraries
   - Install required packages for your platform

3. **Clean Build**
   ```bash
   cargo clean
   cargo build --release
   ```

### Node.js/NPM Issues

**Symptoms:**
- Frontend build failures
- Missing npm packages
- Version compatibility issues

**Solutions:**
1. **Update Node.js**
   - Ensure you're using Node.js 16 or higher
   ```bash
   node --version
   ```

2. **Reinstall Dependencies**
   ```bash
   cd frontend
   rm -rf node_modules package-lock.json
   npm install
   ```

3. **Clear npm Cache**
   ```bash
   npm cache clean --force
   ```

## Performance Issues

### High CPU Usage

**Symptoms:**
- Application consumes excessive CPU resources
- System slowdown
- Fan running loudly

**Solutions:**
1. **Reduce Network Activity**
   - Limit the number of simultaneous connections
   - Reduce message frequency

2. **Check Sync Activity**
   - Messages sync as a bounded, append-only event log between the participating
     devices — they are not broadcast to every peer
   - A very large or frequently-changing conversation can drive repeated sync rounds;
     restart the application if CPU stays pinned

3. **System Monitoring**
   - Use system monitoring tools to identify the cause
   - Consider restarting the application

### High Memory Usage

**Symptoms:**
- Application memory usage keeps growing
- System running low on memory
- Application becomes unresponsive

**Solutions:**
1. **Message Cache**
   - Mesh-Talk caches messages for performance
   - Restart the application to clear the cache

2. **Large Message Volume**
   - Very large messages or high message volume can increase memory usage
   - Consider breaking large messages into smaller chunks

3. **Memory Leaks**
   - If memory usage continues to grow, report the issue
   - Provide detailed reproduction steps

## Advanced Troubleshooting

### Enable Debug Logging

For detailed troubleshooting information, enable debug logging:

```bash
RUST_LOG=debug ./mesh-talk-node --keystore ~/.mesh-talk/node.keystore \
  --password "<pw>" --name "YourName"
```

For even more detailed logging:

```bash
RUST_LOG=trace ./mesh-talk-node --keystore ~/.mesh-talk/node.keystore \
  --password "<pw>" --name "YourName"
```

### Network Diagnostics

1. **Check UDP Multicast Announces**
   ```bash
   # Listen for discovery packets on the multicast port
   sudo tcpdump -i any udp port 47474
   ```

2. **Check TCP Connections**
   ```bash
   # List active TCP connections
   netstat -an | grep ESTABLISHED
   ```

3. **Monitor Network Traffic**
   ```bash
   # Monitor all network traffic from the application
   sudo wireshark
   ```

### Configuration Files

Mesh-Talk anchors its data and configuration under the home directory:

- **macOS / Linux**: `~/.mesh-talk/`
- **Windows**: `%USERPROFILE%\.mesh-talk\`

Per-account encrypted stores live under `~/.mesh-talk/accounts/<user_id>/`. Check these
directories for:
- Encrypted data stores (event logs, device/account keystores, ratchet + sender-key state)
- Per-account history and config (`config_store`)
- Log files

## Getting Help

If you're unable to resolve your issue using this guide:

1. **Check GitHub Issues**
   - Search existing issues: https://github.com/yourusername/mesh-talk/issues
   - If your issue isn't reported, create a new one

2. **Provide Detailed Information**
   When reporting an issue, include:
   - Mesh-Talk version
   - Operating system and version
   - Exact error messages
   - Steps to reproduce the issue
   - Debug logs if applicable

3. **Community Support**
   - Join the Mesh-Talk community discussions
   - Ask questions on the project's GitHub Discussions page

4. **Professional Support**
   - For enterprise deployments, contact the development team
   - Commercial support options are available

## Version Information

This troubleshooting guide applies to Mesh-Talk version 0.1.0.
Last updated: June 22, 2026

For the latest version of this guide, please refer to the official documentation.