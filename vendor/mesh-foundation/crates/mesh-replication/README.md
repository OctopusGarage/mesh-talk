# mesh-foundation-replication

Bounded inventory reconciliation and record retrieval.

Choose the entry point by workload:

- Small, finite sets: `CompleteInventory` and `inventory_session`.
- Indexed incremental repair: `driver::SyncDriver` with an in-memory `RangeIndex`.
- Persistent indexes: `range::RangeRead` and caller-owned checkpoint/commit orchestration.
- Recent/older pages: `history::fetch_page`; page exhaustion is not whole-scope equality.

Run `cargo run -p mesh-foundation-replication --example sqlite_sync` for a real
SQLite reader, atomic opaque-body commits and reopen/idempotency. Its tests cover
rollback, revocation, cancellation and SQL errors. Snapshot preparation is a
small illustrative rebuild, not a scalable persistent index implementation.
No example establishes application trust or supplies a background scheduler.

 There are no default application profiles; optional `mesh-connect` adds session adapters. Authorization and durable stores belong to the host.

## Development and documentation

From the workspace root:

```sh
cargo test --locked -p mesh-foundation-replication
cargo doc --locked -p mesh-foundation-replication --all-features --no-deps --open
```

This crate is part of [Mesh Foundation](https://github.com/OctopusGarage/mesh-foundation).
All seven crates share a version and Git revision. Published on [crates.io](https://crates.io/crates/mesh-foundation-replication) at `0.1.0-alpha.1`.
[API documentation](https://docs.rs/mesh-foundation-replication/0.1.0-alpha.1/mesh_replication/).
Use exact alpha pins, or a reviewed full Git SHA during development. See the delivery
guide for registry aliases and local overrides.

See the repository [README](https://github.com/OctopusGarage/mesh-foundation#readme),
[contribution guide](https://github.com/OctopusGarage/mesh-foundation/blob/main/CONTRIBUTING.md)
and [security policy](https://github.com/OctopusGarage/mesh-foundation/blob/main/SECURITY.md).
Licensed under MIT; see the workspace LICENSE.
