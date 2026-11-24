# SDK and application protocol ownership

Mesh Foundation supplies generic authenticated byte sessions, opaque replication,
cryptographic state machines, encrypted records, mailbox quota mechanisms and
verified blob transfer. It does not own chat events, group permissions or App
lifecycle. The managed vendor copy records its reviewed SDK commit in
`vendor/mesh-foundation/SOURCE`.

`mesh-talk-protocol` is application-owned and not published. It preserves the
original device/account fingerprints and certificate signature domains, signed
chat events and event kind indices, discovery roles and account proofs, legacy
synchronization, pairwise relay envelopes and original mailbox wire/spool bytes.
Adapters in mesh-talk-core import this crate; SDK primitives never depend on it.
No existing identity, encrypted record or wire frame is silently reinterpreted
under the SDK's neutral alpha.2 profiles.

Protocol regression tests retain old behavior and independent golden vectors.
The new crate participates in the actual Rust package inventory and validation
contracts. The application remains on its existing development branch; this is
source migration, not a new application or registry release.
