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


## Explicit application policies

The neutral SDK account has no lifecycle timestamps. This application's existing
account profile retains creation metadata and historical identifiers.
Sender-chain rebasing is an explicit SDK candidate operation; Mesh Talk retains
its existing authenticated recovery rules and gap threshold in the channel owner.
Both in-memory test helpers and durable checkpoint paths propagate candidate errors.
Checkpoint encoding and delayed/consumed-message semantics remain unchanged.

The protocol crate uses the SDK mailbox with default features disabled: it needs
generic quota and finite-window pagination mechanisms. Original chat envelopes, codec/spool profiles,
recipient addressing and expiry scheduling remain application-owned. The SDK's
new typed mailbox ports and optional MFOM2/spool adapters do not alter MTOM1 bytes.

## Shared range core and application recovery

The SDK owns `RangeIndex`, `RangeRead`, canonical summary construction and the
summary frame codec. The application's range module re-exports those mechanisms;
it does not maintain a second ID tree or summary hash implementation. Its
checkpoint still owns blocked ancestors, ordered repair anchors, overlap-based
strategy and finite recovery frontiers. The SQL index implements the SDK reader
port under the existing selection lifetime and encrypted scratch owner. Do not
move account/group authorization or causal repair policy into the SDK.

## Updating the managed SDK

From a clean, committed SDK checkout, run:

```sh
python3 scripts/sdk.py snapshot export ../mesh-talk/vendor/mesh-foundation
python3 scripts/sdk.py snapshot verify ../mesh-talk/vendor/mesh-foundation --source .
```

`SOURCE` and `SNAPSHOT.json` are generated together. Updates reject local edits,
missing/extra files and symlinks. CI runs the bundled verifier offline; explicit
`--source` verification also binds hashes to the recorded SDK Git commit. Offline
hashes alone are not proof against a deliberately forged manifest. Update the
consumer lockfile and run affected range, SQL selection and session tests.
Signed application discovery uses the application's existing profile; mesh-connect
is consumed without its neutral signed-discovery default feature.

SDK SQLite examples are illustrative contract coverage, not this application's
SQL schema or performance benchmark. Forum's inventory/private-mailbox tests
prove independent composition; its production post protocol is a separate owner.

## Historical mailbox paging through the shared engine

The application cursor implements the SDK `DeltaCursor` port and retains its
`first_seen`, account/conversation and epoch qualification. The SDK owns ordinal
advancement, lookahead, upper-window retention and completion/reset transitions.
The application wrapper retains the historical 1500-record request ceiling.
The page is a concrete alias of the generic four-field SDK page; MTOM1 cursor
fields/order, record IDs and existing serialization remain unchanged and covered
by independent compatibility fixtures. First-seen changes still force a reset.

Snapshot updates use an OS-held destination lease and preserve a verified sibling
backup if both installation and rollback fail. Retry from a clean reviewed SDK
checkout verifies recovery state before restoring or cleaning it. Never remove
an unknown backup or treat an existing lease file as proof of a live owner.
The ignored `.mesh-NAME.snapshot-lock` sibling inode coordinates processes even
when their temporary-directory environments differ.

During recovery, an empty target directory is not an installed snapshot and cannot
authorize deletion of a verified backup. Export preserves both and reports the
ambiguous target. Verify the backup, remove only the empty placeholder with
`rmdir`, and retry. Empty directories remain supported for fresh installation
when no recovery backup exists.
