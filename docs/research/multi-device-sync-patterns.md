# Proven patterns for personal messaging and multi-device synchronization

Research date: 2026-10-18. This is design research, not an implementation guide
or a claim about shipped Mesh-Talk behavior. The companion codebase analysis
must determine which capabilities already exist. This research does not assume
that account synchronization needs to be built from scratch.

## Recommendation

**Proposed Mesh-Talk direction:** extend the existing account-addressed event
replication with a visible personal conversation and typed account-private state.
Keep shared conversation membership separate from device replication preferences.
Borrow established patterns; do not adopt a second messaging protocol or a generic
database/file synchronization engine for this feature.

The evidence supports three separate concerns:

1. A personal conversation provides a familiar place to send notes and files.
2. Account-private replication distributes eligible application state to trusted
   devices, including state that does not belong in a conversation timeline.
3. Existing group replication distributes shared events to authorized accounts.
   Each participant independently chooses what their own devices retain.

These are proposed application boundaries, not three independent transports.

## Established architectures

### Signal: personal conversation, device fan-out, separate history bootstrap

Signal's Note to Self is an end-to-end encrypted conversation for personal notes
and sharing messages/files with linked devices. It also supports disappearing
messages. This establishes the product pattern without implying that arbitrary
application settings should be encoded as visible chat messages.
[Signal Note to Self](https://support.signal.org/hc/en-us/articles/360043272451-Note-to-Self)

Signal's January 2025 engineering account describes authenticated QR provisioning,
a one-use linking token, shared account identity keys, and independent device
session keys. Linking also transfers a fresh one-use history key. History travels
as an encrypted archive; attachment pointers allow later retrieval without
re-uploading every large object. Each linked device has its own mailbox. The
article records a 45-day mailbox/attachment availability window at publication.
[Signal engineering article, 2025-01-27](https://signal.org/blog/a-synchronized-start-for-linked-devices/)

The currently retrieved support page offers optional history transfer, including
all chats and the last 45 days of media. It documents five linked devices and
inactivity-related unlinking. Those are Signal operational policies, not necessary
properties of synchronization, and should not become Mesh-Talk defaults merely
because Signal uses them.
[Signal linked-device support, retrieved 2026-10-18](https://support.signal.org/hc/en-us/articles/360007320551-Linked-Devices)

Sesame manages asynchronous device sessions, includes the sender's own account
among recipients, handles device-list changes and concurrent sessions, and
requires bounded retries/storage. It explicitly assumes an authoritative device
directory and per-device server mailboxes. It allows shared per-user or separate
per-device identity keys; authentication remains necessary. It also identifies
restored/orphaned sessions and partial delivery as problems higher layers must
handle.
[Sesame specification, revision 2, 2017-04-14](https://signal.org/docs/specifications/sesame/)

**Mesh-Talk inference:** reuse the personal-conversation UX, sender-device copies,
and separation of bootstrap from ongoing delivery. Do not copy the server-based
device-list reconciliation algorithm unchanged. In a peer network, authenticated
roster versions and propagation rules must replace the directory assumption.
History transfer should export eligible application records, not clone a live
database or another device's advancing ratchet state.

### Matrix: shared rooms, private account data, explicit device trust

Matrix rooms contain typed events and have replicated copies on participating
homeservers. Room privileges constrain which events users may issue. This is a
useful separation between replicated data and permission to author state.
[Matrix rooms and events, updated 2023-02-23](https://matrix.org/docs/matrix-concepts/rooms_and_events/)

The client-server specification separates global/per-room private account data
from room events. Incremental sync uses `next_batch`/`since`; limited timelines
can contain gaps requiring pagination. Transaction IDs make event submission
idempotent. Cross-signing authenticates devices through user signing keys, with
warnings for identity changes. Device deletion invalidates associated access
tokens. These API semantics are not themselves an end-to-end confidentiality
guarantee for ordinary account data.
[Matrix client-server specification, retrieved 2026-10-18](https://spec.matrix.org/latest/client-server-api/)

Matrix's encryption guide describes distinct device keys, pairwise Olm delivery
of group Megolm keys, replay tracking, and group-session rotation. Blocking a
device requires excluding it from future key sharing and invalidating already
exposed outbound sessions. Automatic historical key sharing is recommended only
for verified devices of the same user; other requests require additional checks
or user involvement. Receiving ciphertext therefore does not imply being able to
decrypt it.
[Matrix E2EE implementation guide, updated 2023-02-08](https://matrix.org/docs/matrix-concepts/end-to-end-encryption/)

Matrix federation uses an event DAG and room authorization/state-resolution
machinery to reconcile concurrent histories. That is substantially more than
transporting chat messages between devices.
[Matrix federation specification](https://spec.matrix.org/latest/server-server-api/)

**Mesh-Talk inference:** borrow private account-state namespaces, per-device trust,
idempotency, gap detection, and explicit key availability. Avoid importing
homeservers, Matrix room-state resolution, or Olm/Megolm solely to add this feature.
An existing signed append-only log can support a smaller application-specific
state projection. Authorization operations need their own reviewed ordering;
they must not inherit an arbitrary preference conflict rule.

### Syncthing: explicit replication selection and visible conflicts

Syncthing supports send/receive, send-only, and receive-only folder modes.
Receive-only replicas still redistribute accepted remote changes. Send-only
override and receive-only revert actions are deliberate operations with
potentially destructive effects.
[Folder types](https://docs.syncthing.net/users/foldertypes.html)

Ignore patterns are device-local: `.stignore` itself is not synchronized. A
separate included file can be synchronized to share rules.
[Ignoring files](https://docs.syncthing.net/users/ignoring.html)

Its block exchange protocol carries per-file version vectors and device-local
monotonic sequence numbers. These represent change history and incremental
enumeration respectively; they are different concepts.
[Block Exchange Protocol v1](https://docs.syncthing.net/specs/bep-v1.html)

Concurrent different file contents produce a conflict copy. Modification times
and a device-ID tie-break determine which file receives the conflict name; both
versions remain available rather than being semantically merged.
[Understanding synchronization](https://docs.syncthing.net/users/syncing.html)

Versioning archives files replaced/deleted through remote synchronization; it
does not archive every local edit. It is configured per folder/device and is
disabled by default.
[File versioning](https://docs.syncthing.net/users/versioning.html)

Syncthing authenticates peers using TLS certificate fingerprints against an
accepted-device list. Discovery can expose device IDs and addresses; encrypted
content does not eliminate network metadata.
[Security principles](https://docs.syncthing.net/users/security.html)

A relay forwards encrypted traffic when direct connectivity fails; it observes
addresses, device identities and traffic volume. This is a connection relay, not
an offline content archive.
[Relaying](https://docs.syncthing.net/users/relaying.html)

Syncthing also documents password-encrypted replicas for untrusted storage, but
explicitly labels that feature beta/testing. It is useful evidence for separating
storage custody from content trust, not a suitable basis for claiming a mature
cryptographic subsystem to adopt unchanged.
[Untrusted encrypted devices](https://docs.syncthing.net/users/untrusted.html)

**Mesh-Talk inference:** adopt explicit per-device/per-collection selection,
meaningful progress and conflict reporting. Do not synchronize application
database files, keystores, ratchet databases, caches, or OS-bound paths as ordinary
files. File-copy conflicts cannot preserve a messenger's transactional invariants.
Account-wide preferences and a device's storage/network choices need distinct
ownership.

### Automerge and Yjs: convergence for editable documents

Automerge merges independent property edits and concurrent text/list changes.
Concurrent writes to one property choose a deterministic winner while retaining
other values in an inspectable conflict set. Convergence does not decide which
value best expresses user intent.
[Automerge conflicts](https://automerge.org/docs/reference/documents/conflicts/)

Automerge repositories use pluggable network adapters and may have zero or more
adapters, allowing application-controlled transport.
[Automerge networking](https://automerge.org/docs/reference/repositories/networking/)

Yjs updates are commutative, associative and idempotent; state vectors identify
missing updates. Its binary update merge API does not garbage-collect deleted
content without loading a document.
[Yjs document updates](https://docs.yjs.dev/api/document-updates)

**Mesh-Talk inference:** defer either dependency until a concrete feature needs
concurrent structured/text editing, such as a shared editable note. Immutable
messages require deduplication and stable presentation, not text CRDTs. Scalar
preferences can use small deterministic registers; authorization, deletion,
retention, and device revocation require explicit semantics. CRDT convergence
cannot provide access control, encryption, durable storage, or network reachability.

## Comparison for Mesh-Talk

The fit assessments below are recommendations inferred from the evidence above,
not claims that these systems implement Mesh-Talk's requirements verbatim.

| Pattern       | What to borrow                                                                     | Main trade-off                                                        | What not to copy                                                                       |
| ------------- | ---------------------------------------------------------------------------------- | --------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| Signal        | Personal chat, independent device delivery, optional history bootstrap             | Fan-out grows with device count; history needs an available source    | Primary-phone dependency, fixed retention/device limits, central directory assumptions |
| Matrix        | Typed shared events versus private account state, device trust, gap-aware catch-up | Key distribution/history access remain separate from event delivery   | Homeserver migration and full room-state-resolution machinery                          |
| Syncthing     | Device/collection selection, resumability, explicit conflict outcomes              | A missing replica can prevent availability; file conflicts are coarse | Synchronizing live databases and secrets as files                                      |
| Automerge/Yjs | Concurrent editing within a bounded document                                       | Extra state/history and lifecycle integration                         | Universal CRDT conversion of chat, preferences and security policy                     |

## Selective synchronization must have precise semantics

The following is a proposed Mesh-Talk contract:

| User choice                              | Required behavior                                                          | Boundary                                                                         |
| ---------------------------------------- | -------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| Do not publish this category             | Stop exporting newly created eligible category records from this device    | Other replicas may already hold older copies                                     |
| Do not receive this category             | Exclude it from requested records and from incoming application projection | Never silently advance a category checkpoint past data needed for later backfill |
| Do not download attachments              | Retain authorized message metadata; fetch ciphertext blobs on demand       | Metadata/history presence does not prove blob availability                       |
| Remove local copies                      | Purge only this device's eligible material                                 | Different operation from a replicated deletion event                             |
| Delete across devices                    | Replicate an authorized tombstone with defined conflict rules              | A disconnected or malicious device may keep an old copy                          |
| Keep a device cryptographically excluded | Do not release relevant decryption keys to it                              | A UI filter or shared account secret cannot guarantee this                       |

Policy should distinguish category, conversation scope, history range and device.
Keep mandatory trust/protocol control records outside optional content filters;
otherwise disabling a category could also disable the revocation information
needed to enforce its security. Exclusions apply at bootstrap, incremental export,
incoming application and attachment/key release, not merely at rendering.

Use category-scoped reconciliation state, or an equivalent proven sparse
enumeration scheme. A global timestamp/high-water mark alone is unsafe when
records can be excluded and later re-enabled. Requests also need policy versions
so that a response generated under an earlier policy cannot defeat a newer one.
Unknown categories should not be interpreted as blanket permission.

## Availability, recovery and revocation constraints

These are distributed-system/security inferences for the proposed design:

- Local writes can succeed offline, but another device can synchronize only when
  an authorized reachable peer or a retained encrypted custody copy has the data.
  Never promise immediate synchronization between devices with no overlapping
  reachable path and no store-and-forward custody.
- A bounded post office cannot provide indefinite history recovery. Bootstrap
  needs an online trusted holder or an explicit separately retained encrypted
  backup. Report expired/missing history distinctly from completed synchronization.
- Preserve event IDs across retries and forwarding. An acknowledgement of stored
  ciphertext, application-level acceptance, successful decryption and UI display
  are different states; one device's acknowledgement must not erase another's
  outstanding delivery obligation.
- Snapshot catch-up needs an authenticated boundary and subsequent delta replay.
  Otherwise writes concurrent with export can be omitted. Resume/retry must not
  duplicate records or roll back local operations.
- Enrollment must authenticate a fresh device key and prove possession. A local
  unlock password is not a distributed account authority. Use existing reviewed
  account/device certificates where suitable; QR/fingerprint verification should
  bind the concrete key and linking session.
- A revoked device must not obtain future session/collection keys. Simply dropping
  a connection or hiding the device does not revoke keys already copied to it.
  Rotate affected secrets and propagate authenticated revocation state.
- Revocation becomes effective at a peer after that peer learns valid revocation
  evidence. A partitioned sender cannot know about an unseen revocation; document
  this boundary rather than promise instantaneous global removal.
- Reject stale roster rollback and device-ID/key substitution. Define how
  concurrent enrollment/revocation and offline-authored events are validated.
  Devices carrying a root account signing key may be able to authorize further
  devices; delegating that power changes the compromise/recovery threat model.
- Revocation and tombstones cannot retract plaintext, screenshots, backups or
  decryption keys previously acquired. Security wording must reflect this.

## Minimum protocol reuse and decisions to settle

**Proposed sequence:** first inventory current self-DM, account-sync, device-trust,
event-log and attachment paths. Add the smallest missing personal-conversation
surface. Then extend typed account-private records and selective synchronization
through existing authenticated/versioned transport. Keep group membership and
crypto stable unless a demonstrated gap requires changing them. Treat independent
offline custody and cryptographic category exclusion as separate milestones when
the current infrastructure cannot deliver them safely.

Choose conflict rules by domain: immutable records deduplicate by identity;
independent preference keys merge independently; concurrent writes to one ordinary
preference use a documented logical-order/device tie-break; consequential editable
notes preserve conflict versions; deletion needs a tombstone/resurrection policy;
security changes require explicit authorization and epoch/causal validation.
Wall clocks alone must not determine authority.

Before implementation, settle:

1. Which data categories are supported now, and which are explicitly deferred?
2. Does exclusion mean reduced transfer/storage, or a promise of cryptographic
   isolation from a particular otherwise trusted device?
3. Which preferences are account-wide defaults and which are local overrides?
4. Is historical backfill opt-in, and what survives current pruning/retention?
5. Can a newly authorized device see pre-enrollment conversations, and under what
   group history rules can another account's old keys/content be transferred?
6. Who can enroll/revoke devices, where does the account authority key live, and
   how is authority recovered after all trusted devices are lost?
7. What is the explicitly supported offline interval and custody topology for
   account-private records and attachment blobs?

The practical result should be one synchronization substrate with several
authorized scopes, not three competing engines. This conclusion is a design
recommendation; code inspection and focused compatibility tests must validate it.
