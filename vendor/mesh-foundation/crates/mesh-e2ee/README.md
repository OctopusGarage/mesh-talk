# mesh-foundation-e2ee

Pairwise ratchets, multi-recipient sender keys and checkpoint codecs.

Start with the `ratchet` and `sender_key` modules. Applications own authenticated key distribution and must atomically persist accepted events with receive checkpoints. In-memory candidate checks do not replace durable concurrency control.

## Development and documentation

From the workspace root:

```sh
cargo test --locked -p mesh-foundation-e2ee
cargo doc --locked -p mesh-foundation-e2ee --all-features --no-deps --open
```

This crate is part of [Mesh Foundation](https://github.com/OctopusGarage/mesh-foundation).
All seven crates share a version and Git revision. Published on [crates.io](https://crates.io/crates/mesh-foundation-e2ee) at `0.1.0-alpha.1`.
[API documentation](https://docs.rs/mesh-foundation-e2ee/0.1.0-alpha.1/mesh_e2ee/).
Use exact alpha pins, or a reviewed full Git SHA during development. See the delivery
guide for registry aliases and local overrides.

See the repository [README](https://github.com/OctopusGarage/mesh-foundation#readme),
[contribution guide](https://github.com/OctopusGarage/mesh-foundation/blob/main/CONTRIBUTING.md)
and [security policy](https://github.com/OctopusGarage/mesh-foundation/blob/main/SECURITY.md).
Licensed under MIT; see the workspace LICENSE.

Unreleased alpha.2 exposes explicit sender-chain `prepare_rebase`/`install`.
The host authenticates key distributions and decides when recovery is permitted;
the SDK validates the transition and rejects stale candidates. Persist checkpoints
atomically with application state. Membership, rotation timing and gap-based
recovery thresholds belong to the application.
