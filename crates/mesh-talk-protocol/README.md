# Mesh Talk protocol profile

Application-owned, unpublished compatibility formats for Mesh Talk. This crate
preserves existing event discriminants, signed bytes, account identifiers,
discovery announcements and relay mailbox/spool encodings. These formats are not
part of Mesh Foundation's generic replication or transport interfaces.

- `legacy_event`, `legacy_wire`, `legacy_round`: signed chat event vocabulary and
  existing bounded synchronization protocol.
- `device` and `account`: existing Mesh Talk device/account identifiers,
  certificate signing domains and account creation metadata. They delegate key
  material, generation, signing and strict verification to Foundation; they do
  not call its neutral ID/certificate profile for historical data.
- `discovery`: existing signed announcements, account certificates and relay role.
- `mailbox`: existing recipient-pair envelopes and compatible delta/spool protocol.

The application core owns membership authorization, metadata rules, key release,
retention, recovery scheduling and UI effects. This crate reuses Foundation byte
sessions, generic replication contracts, storage encryption and quota accounting.
It must not introduce a dependency from Foundation back into Mesh Talk.

Run `cargo test -p mesh-talk-protocol`. Compatibility-profile tests pin the old
account identifier, certificate signing domain, event tag and framing magic.

See [ownership and migration](../../docs/sdk-business-boundary.md).
