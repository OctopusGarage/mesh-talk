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

## Application protocols

Signed discovery uses the SDK-neutral `MFAN` v1 profile. Advertisements bind an
application namespace, up to 32 unique protocol/version capabilities, an opaque
metadata payload (at most 2 KiB), a label (at most 1 KiB), keys and endpoint.
Hosts must check the namespace and current trust policy before using a candidate.
A valid signature alone establishes neither authorization nor freshness.
`capability::negotiate` selects the highest overlapping version for one protocol;
the host binds the result to its authenticated session.

Mesh Talk's historic announcements, post-office role and account binding belong
to its application protocol crate. They are not decoded by this profile.
