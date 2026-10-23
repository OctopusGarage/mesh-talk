# Linked-device synchronization design boundaries

For current user behavior, use [Message Yourself and linked devices](settings.md#message-yourself-and-linked-devices).
[Architecture](ARCHITECTURE.md) owns implemented account synchronization and
[network architecture](network-design-reference.md) owns routes and resource
budgets. This document owns extension decisions; it is not an implementation
plan or a record of completed validation.

## Keep the existing replication owners

Personal messages/files and authorized group content reuse existing event,
account-history and encrypted media paths. Message Yourself is the account's own
conversation; it must not depend on another device being discoverable. Group
content still requires accepted membership for each participating device.
Possession of the same account secret does not bypass channel admission.

Keep synchronization selection in a shared core-owned decision function reused
by live sibling fan-out, import and history bootstrap. Do not duplicate category
checks in host adapters or UI components. Device-local preferences select data;
they do not establish cryptographic authority or revoke a device.

A central account service or replacement messaging stack would change the
project's local-first trust model. A general CRDT engine is justified only by an
actual concurrently editable document, not ordinary immutable message history.

## Future private-state replication

Read positions, drafts, portable preferences and typed private records remain
separate extensions to the messaging/files contract. Before implementing them:

- Define an account-private, versioned record and its authenticated writer,
  conflict rule, tombstone and import limits. Do not put private state into a
  shared group event or infer public read receipts from a private read position.
- Keep hardware paths, download destinations, OS permissions and session
  credentials local. Portable appearance choices must tolerate missing packs.
- Preserve concurrent drafts or expose the conflict. A last-write-wins overwrite
  can silently destroy user input. Merge decisions should be pure and testable.
- Keep selection independent in each direction. Turning synchronization off
  stops future transfer; it cannot retract plaintext or records already copied.
- State attachment-byte availability independently of message metadata. Missing
  files need an honest visible state, authorized recovery and integrity checks.

## Offline behavior and retention

A linked device can only obtain retained content from reachable authorized
sources. Local deletion, retention pruning, relay custody and account-wide
redaction are different operations. Tombstones and compaction need explicit
causal/retention rules; a local prune must not silently become remote deletion.

Use existing bounded history pages, authenticated sibling reconciliation and
encrypted persistence. An unavailable peer or expired mailbox cannot imply
complete history. Keep retry state, incomplete transfers and unresolved conflicts
visible without claiming a global synchronization percentage.

## Future account authority V2

Every currently linked device shares account authority. Removing a roster entry
or changing a selection preference cannot revoke a copied signing secret.
Identity-preserving revocation therefore needs a separately reviewed protocol,
not a settings toggle. Until then, suspected compromise requires the documented
new-identity/recovery boundary.

An authority redesign must define distinct management and routine device keys,
versioned enrollment/revocation proofs, key rotation, authenticated device state,
recovery credentials and old-device compatibility. Keep root recovery material
separate from ordinary synchronization; do not make inaccessible devices a
mandatory quorum for recovery.

Offline devices learn revocation only after authenticated evidence reaches them.
Wall-clock claims cannot prove whether an old signed record predates compromise.
Define an accepted frontier and quarantine unaccepted revoked-author records.
Nothing retracts plaintext already copied to another device.

## Acceptance requirements for extensions

Use pure policy/conflict tests and temporary encrypted stores for local rules.
Use loopback devices for authentication, admission, selection and replay; include
restart, retry, malformed pages, denied import and sender exit before download.
Wire or authority changes need explicit version negotiation and old-device
rejection/fallback tests. Physical network and native desktop claims require
separate evidence under [platform validation](platform-validation.md).
