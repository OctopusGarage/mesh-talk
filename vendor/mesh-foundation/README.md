# Mesh Foundation

[![SDK](https://github.com/OctopusGarage/mesh-foundation/actions/workflows/sdk.yml/badge.svg?branch=main)](https://github.com/OctopusGarage/mesh-foundation/actions/workflows/sdk.yml)
[![crates.io](https://img.shields.io/crates/v/mesh-foundation-identity.svg)](https://crates.io/crates/mesh-foundation-identity)
[![docs.rs](https://img.shields.io/docsrs/mesh-foundation-identity/0.1.0-alpha.1)](https://docs.rs/mesh-foundation-identity/0.1.0-alpha.1/mesh_identity/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Rust building blocks for authenticated peer connectivity, bounded replication,
identity, encrypted content, durable storage, offline mailboxes and blob transfer.
Compose the crates you need; your application controls trust, authorization and lifecycle.

**Status:** development alpha. All seven packages are published on crates.io at
`0.1.0-alpha.1`. See the [alpha release](https://github.com/OctopusGarage/mesh-foundation/releases/tag/v0.1.0-alpha.1)
and [delivery guide](docs/sdk-delivery.md) for installation and local development. APIs may change; there is no independent
security-audit claim. Linux, macOS and Windows are covered by the SDK CI matrix.
The tested toolchain is Rust 1.99.0; an MSRV has not been established.

[Quick start](#quick-start) · [Composition](docs/sdk-composition.md) ·
[Delivery](docs/sdk-delivery.md) · [Contributing](CONTRIBUTING.md) ·
[Security](SECURITY.md) · [Changelog](CHANGELOG.md)

Current development targets `0.1.0-alpha.2` (not yet published), with intentionally
breaking [business-neutral interfaces](docs/business-neutral-migration.md). Published
alpha.1 retains its earlier compatibility surface. See
[bounded incremental synchronization](docs/bounded-sync.md) for new turn budgets,
range reconciliation and mailbox acknowledgement APIs. See [recent history](docs/recent-history.md)
for explicit recent/older pages and fallible disk-backed range readers. Installed alpha.1 packages
retain their published behavior.

## Quick start

Install published packages using explicit aliases and exact alpha pins:

```toml
[dependencies]
mesh-identity = { package = "mesh-foundation-identity", version = "=0.1.0-alpha.1" }
mesh-replication = { package = "mesh-foundation-replication", version = "=0.1.0-alpha.1", default-features = false }
```

Rust imports remain `mesh_identity` and `mesh_replication`. Registry installation
does not require GitHub repository access. For development in this checkout:

Install Rust through rustup and Python 3.11 or newer, then run from this checkout:

```sh
cargo run --locked -p mesh-foundation-identity --example device_identity
cargo run --locked -p mesh-foundation-replication --no-default-features --example inventory
cargo test --locked --workspace
cargo doc --locked --workspace --all-features --no-deps --open
```

The examples demonstrate device signing and bounded inventory comparison, with
assertions for rejected tampering and mismatched sets. They do not establish
application trust or authorization. See [security boundaries](docs/security-model.md).

For a development snapshot, use the repository Git URL and pin all SDK crates
to the same full commit SHA using `rev`. See the [consumer guide](docs/sdk-delivery.md#develop-and-consume)
for credential handling, local overrides and upgrades. Until public launch,
GitHub read access is required.

## Crates and features

Crate-local READMEs describe entry points and feature choices. Build rustdoc locally
using the command above; registry package names use the `mesh-foundation-*` prefix.

| Crate | Default features | Optional integration |
| --- | --- | --- |
| [mesh-connect](crates/mesh-connect/README.md) | `signed-discovery` | Disable defaults for session primitives without signed discovery |
| [mesh-replication](crates/mesh-replication/README.md) | None | `mesh-connect` binds transport-independent replication to peer sessions |
| [mesh-blob](crates/mesh-blob/README.md) | None | `mesh-connect` enables session transfer adapters |
| [mesh-identity](crates/mesh-identity/README.md) | None | Device and account signing identities |
| [mesh-e2ee](crates/mesh-e2ee/README.md) | None | Ratchet and sender-key primitives |
| [mesh-storage](crates/mesh-storage/README.md) | None | Encrypted record logs |
| [mesh-mailbox](crates/mesh-mailbox/README.md) | `wire`, `spool` (both default) | Opaque records, bounded retrieval and durable spool |

`test-observability`, where available, is intended for compatibility tests.
Cargo unifies features across a dependency graph; check the resolved graph when
combining adapters.

Published packages and API documentation:

| Package | API docs |
| --- | --- |
| [mesh-foundation-identity](https://crates.io/crates/mesh-foundation-identity) | [mesh_identity](https://docs.rs/mesh-foundation-identity/0.1.0-alpha.1/mesh_identity/) |
| [mesh-foundation-storage](https://crates.io/crates/mesh-foundation-storage) | [mesh_storage](https://docs.rs/mesh-foundation-storage/0.1.0-alpha.1/mesh_storage/) |
| [mesh-foundation-e2ee](https://crates.io/crates/mesh-foundation-e2ee) | [mesh_e2ee](https://docs.rs/mesh-foundation-e2ee/0.1.0-alpha.1/mesh_e2ee/) |
| [mesh-foundation-connect](https://crates.io/crates/mesh-foundation-connect) | [mesh_connect](https://docs.rs/mesh-foundation-connect/0.1.0-alpha.1/mesh_connect/) |
| [mesh-foundation-replication](https://crates.io/crates/mesh-foundation-replication) | [mesh_replication](https://docs.rs/mesh-foundation-replication/0.1.0-alpha.1/mesh_replication/) |
| [mesh-foundation-blob](https://crates.io/crates/mesh-foundation-blob) | [mesh_blob](https://docs.rs/mesh-foundation-blob/0.1.0-alpha.1/mesh_blob/) |
| [mesh-foundation-mailbox](https://crates.io/crates/mesh-foundation-mailbox) | [mesh_mailbox](https://docs.rs/mesh-foundation-mailbox/0.1.0-alpha.1/mesh_mailbox/) |

## Integration overview

See [SDK development and delivery](docs/sdk-delivery.md) for dependency pins,
local overrides, CI verification and release steps. See [SDK composition](docs/sdk-composition.md)
for the dependency graph, optional adapters and integration contracts.

Use `mesh-connect` for pinned-key Noise XX sessions over a reliable, ordered
byte stream. Pass a connected TCP stream or an equivalent transport. Provide
a trusted public key to dial and an admission policy to accept. A failed or
cancelled in-flight frame closes the stream and retires the session. Dial a
new session to retry. The package also exposes
LAN sockets, signed discovery advertisements, and candidate
selection. A host runs discovery, provides its trust policy and schedules
reconnects. Mesh Talk composes the same frame primitives with its existing
application handshake and session pooling.

`mesh-replication` provides bounded ID inventory assembly, canonical digests,
and exact set differences. Its generic primitives are independent of
`mesh-connect`. Enable the optional `mesh-connect` feature for direct
composition with a pinned session. Replication has no default application profile;
its records are opaque and all application authorization comes from host adapters.
The generic session helpers own bounded inventory and export framing. Callers supply
the current authorized store, reserve memory for the remote inventory before
its first read and supply an authenticated session.

Run `cargo test --workspace` to exercise real loopback TCP and peer-key
rejection.

## Package boundaries

| Package | Responsibility | Extraction status |
| --- | --- | --- |
| `mesh-connect` | Authenticated frame sessions, LAN discovery, route selection and connection lifecycle | Pinned-key sessions, LAN/IPv6 sockets, signed discovery codecs and candidate selection available. Hosts own routing configuration and relay coordination. |
| `mesh-replication` | Scoped event inventories, bounded reconciliation, resumable export and import | Inventory, exact differences, bounded requester and responder, and generic session codecs available. Applications supply authorized persistence via adapters. |
| `mesh-e2ee` | Content encryption state machines and checkpoint codecs | Pairwise ratchets, group sender keys and checkpoints available. In-memory installation rejects stale candidates. Hosts own key distribution and must compare the persisted checkpoint before committing a durable candidate. |
| `mesh-identity` | Signing, credential verification, and separate node and author identities | Device and account identities plus device certificate verification available. Account trust, enrollment and recovery policy belong to the application. |
| `mesh-storage` | Encrypted record framing, durable append and atomic replacement | Record encryption and durable append/rewrite available. Applications own indexed data models and profile lifecycle. |
| `mesh-mailbox` | Opaque scoped deposits, bounded retrieval and configurable storage | Typed store/transport ports and budgeted synchronization are independent of optional wire/spool profiles. Hosts own authentication, retention policy, scheduling and federation. |
| `mesh-blob` | Content verification, bounded chunk transfer and resumable retrieval | Chunk cryptography and ordered stream verification available. Hosts stage bytes until verification succeeds and own transfer scheduling, manifests, consent and retention. |

Replication owns its session and store contracts. A composition adapter binds a
connection and a store to those contracts. The replication engine handles signed
public records and encrypted records without access to content decryption keys.
Identity supplies verified principals, while each application supplies the
policy deciding what those principals may read, write and export.

Extract implementations alongside their compatibility tests. Mesh Talk adapters
preserve existing wire formats, event IDs and persisted checkpoints. Validate
packages through a forum consumer and existing Mesh Talk paths before publishing
a stable interface. For encrypted content, preserve the atomic commit of an
accepted event and its updated receive state across failures and restarts.

Application-owned compatibility codecs live in Mesh Talk's `mesh-talk-protocol`,
not in this SDK. Forum consumers define their own post/revision schemas. See the
[business-neutral alpha.2 migration](docs/business-neutral-migration.md) for changed
identity domains, discovery and mailbox profiles, and preserved application bytes.

## Community and license

See [Contributing](CONTRIBUTING.md), [Support](SUPPORT.md) and
[Governance](GOVERNANCE.md) for collaboration and maintenance expectations.
Licensed under [MIT](LICENSE). Public-launch preparation is tracked in the
[readiness checklist](docs/open-source-checklist.md); local results are recorded in
[verification evidence](docs/open-source-verification.md).
