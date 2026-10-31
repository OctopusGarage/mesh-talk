# mesh-talk-core

The UI-free protocol core of [Mesh-Talk](https://github.com/OctopusGarage/mesh-talk) — a
**local-first, end-to-end-encrypted messenger**. This crate is the foundation an
SDK is built on: it has **no UI and no Tauri dependency**, so it can be embedded in a CLI,
a daemon, a desktop shell (as the Mesh-Talk app does), or any other Rust program.

No vendor-operated service is required. Peers discover each other on the local network (signed UDP **multicast**
announces — group `224.0.0.167`, default port `47474` — plus a `/24` unicast scan fallback),
and use optional routed signed queries for candidate endpoints. They
talk directly over an authenticated, encrypted Noise channel, and converge a signed,
content-addressed event log. Optional equal post offices hold recipient-encrypted opaque events temporarily,
subject to capacity and a 30-day maximum age. Opt-in bridge directories and explicit
mutual federation trust extend introductions and custody across reachable sites. DMs can fan out to certified account devices; group delivery follows accepted
membership and the separate certified-sibling enrollment rules.

## What's inside

| Module                          | Responsibility                                                                                                                                                                               |
| ------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`identity`](src/identity/)     | Ed25519/X25519 device identities, cross-device accounts, and device certificates.                                                                                                            |
| [`eventlog`](src/eventlog/)     | Content-addressed, hash-linked, encrypted event log + peer reconciliation (sync).                                                                                                            |
| [`ratchet`](src/ratchet/)       | Double Ratchet — per-message forward secrecy for DMs.                                                                                                                                        |
| [`channel`](src/channel/)       | Sender-key group ratchet — forward secrecy for group channels.                                                                                                                               |
| [`dm`](src/dm.rs)               | Sealed direct-message envelopes.                                                                                                                                                             |
| [`transport`](src/transport/)   | A Noise (`snow`) secure channel with identity binding over TCP (dual-stack); `transport::net` holds the low-level socket + multicast-join helpers shared by the CLI and the desktop runtime. |
| [`discovery`](src/discovery/)   | Signed LAN/routed discovery, interface scope, bounded roster and route provenance.                                                                                                           |
| [`postoffice`](src/postoffice/) | Bounded opaque custody, eligible presence and mutually trusted equal-relay federation.                                                                                                       |
| [`file`](src/file/)             | Chunked, per-chunk-AEAD file transfer with resume + a versioned manifest (48 KiB chunks; files up to ~4 GiB).                                                                                |
| [`storage`](src/storage/)       | At-rest crypto: PBKDF2/AES-GCM encryption and `EncryptedRecordLog` (`storage::record_log`), the generic append-only encrypted log every on-disk sidecar store is built on.                   |
| [`recovery`](src/recovery/)     | Versioned, authenticated `.mtbackup` archive and logical snapshot primitives for manual backup and restore.                                                                                  |
| [`node`](src/node/)             | The runtime that wires the above into a working peer (DMs, channels, files, history, sync).                                                                                                  |

Wire formats are `bincode` and versioned (magic byte + version) for forward-compatible parsing.

## Use it

This crate is not published to crates.io; depend on it by git (or path, within the workspace):

```toml
[dependencies]
mesh-talk-core = { git = "https://github.com/OctopusGarage/mesh-talk" }
tokio = { version = "1", features = ["full"] }
```

### Example (abridged)

Load a persistent identity, open a node, and send a direct message. This is the shape of a
node; for a complete, runnable program see [`src/bin/mesh-talk-node.rs`](src/bin/mesh-talk-node.rs),
the reference consumer this crate ships.

```rust,ignore
use std::sync::{Arc, Mutex};
use mesh_talk_core::discovery::Roster;
use mesh_talk_core::identity::{keystore, account_keystore};
use mesh_talk_core::node::{Node, ReceivedDm};
use tokio::sync::mpsc;

// Persistent, password-encrypted device + account keystores (created on first run).
let identity = keystore::load_or_create("device.keystore".as_ref(), "pw")?;
let account = account_keystore::load_or_create("account.keystore".as_ref(), "pw")?;

// Discovery writes the roster; the node reads it. Inbound events arrive on channels.
let roster = Arc::new(Mutex::new(Roster::default()));
let (dm_tx, mut dm_rx) = mpsc::unbounded_channel::<ReceivedDm>();
let (chan_tx, _chan_rx) = mpsc::unbounded_channel();
let (file_tx, _file_rx) = mpsc::unbounded_channel();

let node = Node::open_with_account(
    identity, account, Arc::clone(&roster),
    dm_tx, chan_tx, file_tx,
    "messages.log".as_ref(), "sent.log".as_ref(), "pw",
)?;

// `node.send_dm(&recipient_user_id, b"hello").await?;` once a peer is in the roster.
// Received DMs surface on `dm_rx`; run discovery + the accept loop to populate the roster.
// (See the CLI for the discovery/listener/drain wiring.)
```

The crate is async and built on Tokio. `NodeRuntime` starts discovery, the accept
loop, and delivery tasks when a host runs the node.

Custom `eventlog::SyncStore` implementations return `Result<_, LogError>` from
storage reads, including capability, fingerprint, consent, custody and event export
queries. Reconciliation constructors and handlers also return `Result`; propagate
local storage failures rather than replacing them with empty history or a negative
lookup. Network sessions expose these failures as `SessionError::Store` with their
source chain. Use `LogError::AdmissionRejected` for explicit incoming-event policy
denials; ordinary I/O errors abort the exchange. A failed batch may have committed
a prefix, so retries must retain content-addressed duplicate handling. Event IDs
and wire serialization are unchanged. See the [protocol guide](../../docs/ARCHITECTURE.md)
for reconciliation and durability contracts.

Individual durable lookups, `PersistentEventLog::get` and `authored_event`, return
`Result<Option<Event>, LogError>`. A successful event owns its complete signed
bytes and can outlive the log or its mutex guard; `Ok(None)` means absence, while
`Err` means the lookup failed. Propagate failure before selecting a fallback proof,
authorizing a transfer or reclaiming media. The explicit in-memory `EventLog`
retains its borrowed lookup API. This ownership boundary alone does not replace
the existing resident history indexes or startup replay with demand loading.

Affected `Node` history and search methods, protected-content queries, attachment
consent and save-readiness queries, and channel custody counts also return
`Result` values. `attachment_consent_status` exposes `LogError`; the other listed
node queries use `NodeError`. Matching `NodeRuntime` history/search methods propagate
these failures. Use `?` before consuming their existing successful values;
absence or a zero count must not stand in for a failed read. Typed node/runtime
errors retain their sources. Host IPC keeps its successful JSON payloads and
reports read failures through its existing error channel. Boolean Favorites
eligibility remains a conservative boundary: it logs a failed canonical read
and denies capture/export eligibility without claiming the content is absent.
An outgoing request already durably accepted into the delivery journal remains
accepted if subsequent projection/recovery reads fail. Its supervisor logs the
failure and retains queued work for retry; acceptance does not assert a send or
recipient delivery. Query failures still use the error channel above.

Exhaustive `LogError` matches must also handle indexed-read failures:
`StaleCursor`, `InvalidPageBudget`, `PageBudgetTooSmall`,
`RepositoryCounterExhausted` and `IncompatibleEventSchema`. These are local read
or repository-state errors, not invalid incoming events. Raw Lamport pagination
does not provide causal ordering; use the ordering contract of the selected API.

## CLI: `mesh-talk-node`

A headless node ships with the crate as a reference consumer:

```bash
# A normal node: discovers peers on the LAN and runs a 1:1 DM REPL.
cargo run --bin mesh-talk-node -- \
    --keystore ./alice/device.keystore --password pw --name Alice

# A post office: a durable store-and-forward relay (no REPL, relays ciphertext).
cargo run --bin mesh-talk-node -- \
    --keystore ./relay/device.keystore --password pw --name relay --post-office
```

REPL commands: `/peers`, `/msg <user_id-prefix> <text>`, `/history <user_id-prefix> [n]`, `/quit`.

> Production startup derives keys for independent encrypted stores; timing depends
> on the machine. Wait for `post-office … listening` rather than a fixed sleep.
> Test-only `fast-test-kdf` builds must not be deployed as permanent services.

## Tests

```bash
# Fast unit + in-process integration tests (KDFs run with cheap test params).
cargo test -p mesh-talk-core

# Heavy, real-process end-to-end rigs (spawn actual `mesh-talk-node` processes that
# discover each other over UDP and talk over TCP). They are `#[ignore]`d by default;
# run them serially via the workspace Makefile:
make e2e
```

During development, select the affected `--lib <filter>` or `--test <target>`
only. `make e2e` is the explicit comprehensive backend rig defined by the root
[Makefile](../../Makefile); do not copy its target list into another runner. See
[the testing workflow](../../docs/testing-feedback-loop.md) for escalation triggers.

## Security model (summary)

- **End-to-end encryption.** Message content is encrypted to the recipient(s); relays and the
  network never see plaintext and hold no conversation keys.
- **Forward secrecy.** DMs use a Double Ratchet; group channels use a sender-key ratchet with
  epoch rotation on membership change.
- **Authenticated transport.** Sessions run over Noise with the peer's device identity bound
  into the handshake (canonical, `verify_strict` Ed25519 signatures).
- **Integrity.** The event log is content-addressed and hash-linked; the at-rest log is
  AEAD-authenticated. Recovery requires an authorized retained source or backup; it
  must not silently treat corruption as valid history.
- **Metadata limits.** Relays see connecting identities, pseudonymous mailbox IDs,
  size buckets and timing; original signed-event headers are inside encrypted
  wrappers. See [relay privacy](../../docs/post-office-metadata-privacy.md).
- **Feature boundaries.** Calls use live direct signaling and separate media;
  sibling replay has its own authenticated route. Neither follows automatically
  from mailbox discovery. See the [network baseline](../../docs/network-design-reference.md).

This is research-grade software; it has not had an independent security audit.

## License

MIT. Part of the [Mesh-Talk](https://github.com/OctopusGarage/mesh-talk) workspace; see
[`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the full design.

[`identity`](src/identity/): src/identity
[`eventlog`](src/eventlog/): src/eventlog
[`ratchet`](src/ratchet/): src/ratchet
[`channel`](src/channel/): src/channel
[`dm`](src/dm.rs): src/dm.rs
[`transport`](src/transport/): src/transport
[`discovery`](src/discovery/): src/discovery
[`postoffice`](src/postoffice/): src/postoffice
[`file`](src/file/): src/file
[`storage`](src/storage/): src/storage
[`recovery`](src/recovery/): src/recovery
[`node`](src/node/): src/node
