# mesh-foundation-connect

Authenticated peer sessions and signed LAN discovery.

Start with the `session` module for pinned-key streams. Hosts provide trusted peer keys, admission policy, deadlines and reconnect scheduling. Default `signed-discovery` enables signed discovery codecs; disable default features for session-only use.

## Development and documentation

From the workspace root:

```sh
cargo test --locked -p mesh-foundation-connect
cargo doc --locked -p mesh-foundation-connect --all-features --no-deps --open
```

This crate is part of [Mesh Foundation](https://github.com/OctopusGarage/mesh-foundation).
All seven crates share a version and Git revision. Published on [crates.io](https://crates.io/crates/mesh-foundation-connect) at `0.1.0-alpha.1`.
[API documentation](https://docs.rs/mesh-foundation-connect/0.1.0-alpha.1/mesh_connect/).
Use exact alpha pins, or a reviewed full Git SHA during development. See the delivery
guide for registry aliases and local overrides.

See the repository [README](https://github.com/OctopusGarage/mesh-foundation#readme),
[contribution guide](https://github.com/OctopusGarage/mesh-foundation/blob/main/CONTRIBUTING.md)
and [security policy](https://github.com/OctopusGarage/mesh-foundation/blob/main/SECURITY.md).
Licensed under MIT; see the workspace LICENSE.
