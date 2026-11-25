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

## Cryptographic compatibility profile

The current ratchet and sender-key mechanisms retain the historical HKDF info
labels `MeshTalk-Ratchet-RK`, `MeshTalk-Ratchet-CK` and `MeshTalk-Ratchet-MSG`.
They select cryptographic derivations, not application schemas or permissions.
Changing these bytes changes keys and makes existing peers/data incompatible.
Independent known-answer tests pin all three derivations.

The sender receiver-checkpoint prefix `MTRX\x01` is likewise an existing local
format identifier. It does not make the checkpoint a chat event or account backup.
Keep it stable when restoring existing encrypted checkpoints.

Hosts must explicitly agree on compatible cryptographic formats before exchanging
content. This crate does not negotiate profiles automatically. A future profile
requires a distinct version, authenticated selection and a migration plan; arbitrary
runtime KDF-label configuration is intentionally not exposed. Key distribution,
application record encoding, membership and recovery policy remain host-owned.
