# SDK and application protocol ownership

Mesh Foundation supplies generic authenticated byte sessions, opaque replication,
cryptographic state machines, encrypted records, mailbox quota mechanisms and
verified blob transfer. It does not own chat events, group permissions or App
lifecycle. The consumer pins the coordinated SDK packages to `=0.1.0-alpha.2` on crates.io;
Cargo.lock records their registry checksums.

`mesh-talk-protocol` is application-owned and not published. It preserves the
original device/account fingerprints and certificate signature domains, signed
chat events and event kind indices, discovery roles and account proofs, legacy
synchronization, pairwise relay envelopes and original mailbox wire/spool bytes.
Adapters in mesh-talk-core import this crate; SDK primitives never depend on it.
The historical device/account types are thin owners of profile and lifecycle
metadata: Foundation owns secret material, key generation, cached public-key
construction, signing and strict verification. Historical certificate input is
constructed here and signed with the SDK primitive; neutral SDK certificate/ID
profiles must never reinterpret existing application bytes.
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
summary frame codec. `ReaderBinding` and `SummaryFrame::validate_for` own the
shared snapshot/frame admission rules, including scope, authority, revision,
pinned view and frontier qualification. Storage errors remain errors and failed
reads do not advance the checkpoint or overlap counts. The application's range
module re-exports those mechanisms without maintaining a second ID tree or
summary hash implementation. Compressed-prefix reconstruction uses the SDK's
`Prefix::common` and checked `Summary::lift`; the SQL adapter maintains its own
transactional rows without another canonical lifting algorithm. Its checkpoint
still owns blocked ancestors, ordered repair anchors, overlap-based
strategy and finite recovery frontiers. The SQL index implements the SDK reader
port under the existing selection lifetime and encrypted scratch owner. Do not
move account/group authorization or causal repair policy into the SDK.

Local-user credential codecs also use SDK signing material. Local user UUIDs,
login/password policy, Base64 public keys and raw 32-byte secret storage remain
application-owned and unchanged. The unpublished `KeyManager` Rust helper now
accepts/returns the SDK signer rather than a dalek secret-key object; known
workspace consumers infer that type. This source-level helper change does not
change account/device identifiers, wire formats or credential records.

## Updating the SDK

Upgrade all Mesh Foundation dependencies together, regenerate Cargo.lock and run
session, range and SQL selection tests. Published alphas are immutable and exact
pins prevent unintended prerelease upgrades.

For local development with a matching SDK version:

```sh
python3 ../mesh-foundation/scripts/sdk.py patch --source crates-io > /tmp/mesh-talk-sdk.toml
cargo --config /tmp/mesh-talk-sdk.toml test -p mesh-talk-core --lib
```

The override is temporary and stays outside the repository. Restore lockfile
changes caused by local overrides and validate without the override before committing.

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

## Persistent reader operation contracts

The SDK also offers `reader::prepare` for standard bounded reader discovery.
Prepared turns release continuation only after the host's synchronous atomic
commit succeeds. The production SQL index is exercised against this contract
with actual repository transactions, rejection, retry and selection retirement.
The SDK's opt-in `test_support::check_reader` also checks this real SQL adapter
against canonical summaries and membership probes; it is development-only and
does not replace application-specific authorization/transaction tests.
This is adapter conformance evidence, not a claim that production causal repair
uses the standard SDK checkpoint. That recovery owner continues to use shared
reader/frame qualification with its application-specific checkpoint.
