# Native Bluetooth transport

Native BLE is an experimental direct-link transport, separate from the released
UDP multicast discovery and TCP node runtime. It is currently a two-host macOS
rehearsal, not an enabled desktop chat feature. It does not interoperate with
Bitchat. Pure PWA peripheral support is outside this capability.

## Ownership and authentication

`transport/ble.rs` owns the bounded session-scoped chunk/byte-stream boundary.
`SecureChannel` remains the owner of Noise XX and device identity binding. Native
adapters must reserve inbound capacity before copying callback data and close a
link on budget exhaustion; callbacks must never wait for a consumer or hold node
locks. Shutdown revokes queued data as well as future callbacks. The byte stream's
flush acknowledges queue admission, not radio acknowledgment or durable delivery.

`src-tauri/examples/bluetooth-radio.swift` owns a single CoreBluetooth central or
peripheral connection for the rehearsal. GATT writes use response acknowledgment;
notifications wait for native readiness after backpressure. Chunks are at most
20 bytes. One pending outbound chunk and 64 inbound output slots bound buffering.
A dedicated stdout writer prevents a blocked consumer from freezing Bluetooth
callbacks or signal handling. Disconnect, power loss and malformed or excessive
input fail the process; the caller must start a new session to retry.

`bluetooth-rehearsal` owns the explicit process lifecycle, two-minute deadline,
pre-exchanged public-key pinning and encrypted echo. The helper sees encrypted
Noise bytes, not chat plaintext. Its process and streams are retired on failure,
Ctrl-C/SIGTERM/SIGHUP or completion. The native helper also watches its owning
process once per second and exits if that owner dies, including abrupt kills. Test text is limited to 4096 bytes and is held in memory;
an echo is not a persisted message, account proof, read receipt or delivery claim.
Use dedicated rehearsal identities, not production account keystores.

## Two-host macOS rehearsal

Build on each host from the repository root (Xcode Command Line Tools required):

```sh
mkdir -p tmp/bluetooth
swiftc -parse-as-library src-tauri/examples/bluetooth-radio.swift -o tmp/bluetooth/radio \
  -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist \
  -Xlinker src-tauri/examples/bluetooth-radio.plist
cargo build -p mesh-talk-core --example bluetooth-rehearsal
```

Run the radio-free batch regression without requesting Bluetooth access:

```sh
swiftc -parse-as-library -D BLE_REHEARSAL_TEST \
  src-tauri/examples/bluetooth-radio.swift src-tauri/examples/bluetooth-radio-tests.swift \
  -o tmp/bluetooth/batch-tests
tmp/bluetooth/batch-tests
```

Create a private password file at `tmp/bluetooth/password` using a local editor,
then run `chmod 600 tmp/bluetooth/password`. Create and print each host's dedicated
public identity (this command does not start Bluetooth):

```sh
cargo run -p mesh-talk-core --example bluetooth-rehearsal -- \
  --keystore tmp/bluetooth/identity --password-file tmp/bluetooth/password --identity
```

Exchange the printed public keys through an authenticated out-of-band channel.
On host A, run the following with host B's key; on host B use `--role central`
and host A's key. Allow the operating system's Bluetooth permission request.

```sh
cargo run -p mesh-talk-core --example bluetooth-rehearsal -- \
  --keystore tmp/bluetooth/identity --password-file tmp/bluetooth/password \
  --radio tmp/bluetooth/radio --role peripheral --peer PEER_PUBLIC_KEYS
```

Disable IP networking while retaining Bluetooth for the BLE-only scenario. Verify
an authenticated text/echo, mismatch rejection, interruption, owner death, Bluetooth off/on,
and a fresh-process reconnect. Record the host hardware, OS, permission state,
foreground/background state and exact result. One Mac cannot establish two-host
radio evidence by connecting to its own advertisement. A typecheck or mock stream
test is not evidence of native radio behavior or absence of all deadlocks.

## Desktop integration boundary

Do not insert synthetic IP addresses into the roster or proxy BLE through a
loopback socket. Discovery is an untrusted endpoint hint, not authenticated
presence. Full node integration requires typed TCP/BLE routes and runtime
generations, common privacy admission on dial and accept, existing connection
permits, transport-aware disclosure, and receipt handling in existing owners.
TCP pooling/liveness relies on socket readiness; fresh BLE sessions must not be
pooled under approximate TCP semantics. Prefer a reachable TCP route before BLE,
within a shared bounded operation budget. Logout, disable and runtime replacement
must revoke authority and retire all adapter work before publishing a replacement.

See [network contracts](network-design-reference.md),
[module ownership](module-boundaries.md), and
[reliability rules](ai-coding-reliability.md). Mobile platforms, multi-hop relay,
background guarantees and automatic desktop routing require separate evidence.
