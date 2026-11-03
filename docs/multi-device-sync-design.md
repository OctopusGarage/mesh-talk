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
content requires accepted account membership and a verified device acting under
that account's current grant. Linked devices inherit their account's membership
and role; the group owner does not approve each device. Legacy device-only groups
require a compatible owner-authorized account-grant upgrade before this can be
established without the original owner's device online.

Keep synchronization selection in a shared core-owned decision function reused
by live sibling fan-out, import and history bootstrap. Do not duplicate category
checks in host adapters or UI components. Device-local preferences select data;
they do not establish cryptographic authority or revoke a device.

A central account service or replacement messaging stack would change the
project's local-first trust model. A general CRDT engine is justified only by an
actual concurrently editable document, not ordinary immutable message history.

## Account and device decision boundaries

| Concern            | Account decision                                                                      | Device responsibility                                                                    |
| ------------------ | ------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Identity           | Login credentials, nickname, avatar and bio                                           | Local storage namespace, encrypted storage keys and active session                       |
| Contacts           | Contact account, remarks, pins and visibility                                         | Reachable routes and individual device certificates                                      |
| Groups             | Invitations, accepted membership, ownership, roles, removal and departure             | Signed event provenance, authenticated inheritance and independent sender keys           |
| Group presentation | One member and avatar per verified account; shared group directory                    | Presence evidence and availability of retained copies                                    |
| Messages           | Account conversation, own-message identity and authorized account actions             | Transport acknowledgements, original event signatures and verified attachment bytes      |
| Preferences        | Portable account preferences and private libraries under their synchronization policy | Download paths, OS permissions, autostart, saved-login permission and local-agent grants |
| Calls              | Ring the contact account                                                              | Authenticate the chosen answering device and bind its media session                      |

Every user-facing entry point must reuse the owning account rule, including group
creation. A selected device is only a route to authenticate the invited account;
matching names do not prove matching accounts. New desktop groups commit the
existing account-authority format at creation rather than waiting for a legacy
migration. The older creation primitive remains available for compatibility
fixtures and SDK consumers that explicitly use device-only groups.

Reference member snapshots may supply names, avatars and member counts without
manufacturing local membership or management rights. Missing or incomplete facts
remain unknown. Account-private sync selection does not turn group ownership
into a device permission, and an unavailable route does not revoke membership.

## Private-state replication

Portable contact remarks, pins/visibility and supported appearance choices use
account-bound preference records. Shared login credentials use a separate private
account-access record and encrypted local credential envelopes. Message Favorites
have their own bounded replay and tombstones. These owners must not be replaced
by another generic synchronization engine.

Read positions, drafts, custom group photos, installed sticker libraries and their
private-use choices use separate account-private records alongside messaging and
files. Maintain these contracts when extending their existing owners:

- Define an account-private, versioned record and its authenticated writer,
  conflict rule, tombstone and import limits. Do not put private state into a
  shared group event or infer public read receipts from a private read position.
- Keep hardware paths, download destinations, OS permissions and saved-login
  permission local. Shared account login credentials remain separate from each
  device's retained storage keys and active session. Portable appearance choices
  must tolerate missing packs.
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

Authenticated exact-device transitions can repair a foreign contact's previous
account pin. They do not retire an existing grant in this node's own certified
sibling registry. A departing certified sibling therefore remains blocked until
a separate revocation protocol can durably retire its grant and reject stale
certificate imports. Never relax private account-export checks or alias account
roots to bypass this boundary.

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
