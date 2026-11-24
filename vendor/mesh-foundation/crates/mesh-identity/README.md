# mesh-foundation-identity

Device signing identities and account-issued device certificates.

Start with `device::DeviceIdentity` and `account::Account`. Verify certificates against the expected device key; applications establish account trust, revocation policy and secure secret storage.

## Development and documentation

From the workspace root:

```sh
cargo test --locked -p mesh-foundation-identity
cargo doc --locked -p mesh-foundation-identity --all-features --no-deps --open
```

This crate is part of [Mesh Foundation](https://github.com/OctopusGarage/mesh-foundation).
All seven crates share a version and Git revision. Published on [crates.io](https://crates.io/crates/mesh-foundation-identity) at `0.1.0-alpha.1`.
[API documentation](https://docs.rs/mesh-foundation-identity/0.1.0-alpha.1/mesh_identity/).
Use exact alpha pins, or a reviewed full Git SHA during development. See the delivery
guide for registry aliases and local overrides.

See the repository [README](https://github.com/OctopusGarage/mesh-foundation#readme),
[contribution guide](https://github.com/OctopusGarage/mesh-foundation/blob/main/CONTRIBUTING.md)
and [security policy](https://github.com/OctopusGarage/mesh-foundation/blob/main/SECURITY.md).
Licensed under MIT; see the workspace LICENSE.
