# mesh-foundation-mailbox

Opaque scoped records, negotiated delta retrieval and durable mailbox spools.

Start with `Record::new(scope, bytes)`, `ports::MailboxTransport` and
`client::sync_turn`. The typed engine has no codec, filesystem or scheduler. Hosts
own payload authentication, admission, retention, scheduling and federation.

Optional default features are `wire` (MFOM2 codecs, service and session adapter)
and `spool` (encrypted durable storage). They can be selected independently.
`SpoolOptions` separates record/query limits from wire framing and selects manual
or first-window-age retention. Atomic body/cursor commits and current access
checks remain host responsibilities. See [composition](../../docs/sdk-composition.md)
for budgets, source binding and integration order.

## Development and documentation

From the workspace root:

```sh
cargo test --locked -p mesh-foundation-mailbox
cargo doc --locked -p mesh-foundation-mailbox --all-features --no-deps --open
```

This crate is part of [Mesh Foundation](https://github.com/OctopusGarage/mesh-foundation).
All seven crates share a version and Git revision. Published on [crates.io](https://crates.io/crates/mesh-foundation-mailbox) at `0.1.0-alpha.1`.
[API documentation](https://docs.rs/mesh-foundation-mailbox/0.1.0-alpha.1/mesh_mailbox/).
Use exact alpha pins, or a reviewed full Git SHA during development. See the delivery
guide for registry aliases and local overrides.

See the repository [README](https://github.com/OctopusGarage/mesh-foundation#readme),
[contribution guide](https://github.com/OctopusGarage/mesh-foundation/blob/main/CONTRIBUTING.md)
and [security policy](https://github.com/OctopusGarage/mesh-foundation/blob/main/SECURITY.md).
Licensed under MIT; see the workspace LICENSE.

This page describes unreleased alpha.2. The old chat envelope and wire/spool
profiles are application-owned; see [migration](../../docs/business-neutral-migration.md).
Run `cargo run -p mesh-foundation-mailbox --no-default-features --example typed_mailbox`
for a carrier-independent example, or enable `spool` and run `--example opaque_mailbox`
for durable storage. Content integrity is not authentication or permission.
