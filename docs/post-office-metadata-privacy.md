# Post-office metadata privacy

## Current relay boundary

New clients wrap each signed event in a recipient-encrypted **opaque outer
event** before syncing to a post office. The outer event has a pairwise,
relay-specific mailbox ID, a pairwise pseudonymous signing key, an opaque
sequence number, no parents, zero logical and wall-clock timestamps, and a
constant `Message` kind. The original signed event, including its author,
conversation ID, kind, parents, timestamps, channel controls, file metadata,
and ciphertext, is inside AES-256-GCM ciphertext. Relay storage and sync see
only the outer event. Direct peer sync still exchanges the original signed
events over authenticated Noise.

The mailbox ID, AEAD key, nonce-derivation key, and pseudonymous signing seed
come from a domain-separated HKDF over the pair's X25519 static DH secret,
both complete public device identities in canonical order, and the relay's
Ed25519 key.
Both devices derive the same mailbox without registering a token. A third
device and the relay cannot derive it. Each inner event's signed 256-bit ID
derives a deterministic AES-GCM nonce under the mailbox nonce key. Since the
event ID commits to the whole signed event, a repeated upload produces the
same outer event and the relay deduplicates it. Distinct events sharing a
nonce would require a cryptographic collision. The recipient verifies both
outer and inner signatures, the AEAD tag, the derived nonce, and the inner
event's ordinary causal and privacy admission rules. Invalid or incomplete
inner history is not surfaced.

The encrypted inner object is length-prefixed and padded to one of six size
buckets (512 B through 60 KiB); oversized objects cannot use the relay.
Padding reduces size detail but cannot hide traffic timing, packet count,
or the bucket. File chunks use the same wrapper as messages and channel
controls. The production relay rejects old clear-header event shapes.
New clients also refuse ordinary inbound sync requests from a relay, so a
relay cannot ask for the local clear event log. A new client can upload its
opaque wrapper to an older relay because the outer event uses the existing
signed sync wire. Old clients cannot upload their clear events to a new
production relay. No automatic clear-metadata fallback exists.

## What remains visible

| Relay observation | Why it remains visible |
| --- | --- |
| Authenticated connecting device and IP address | Noise identity authentication and TCP routing |
| Pairwise opaque mailbox ID | Selecting the stored queue for sync |
| Pseudonymous outer author and opaque event ID | Relay event-log integrity and deduplication |
| Size bucket, count, upload/pull time | Storage, traffic, and retrieval behavior |

The relay can correlate uploads and pulls to infer who communicates with
whom, even though the mailbox ID has no readable identity or conversation
field. This is **metadata minimization**, not anonymous communication.
Encrypting routing fields to the relay would not hide them from that relay;
removing connection linkage would require an anonymity system with different
transport and retrieval assumptions. A relay can still delay, withhold,
delete, or replay opaque events. The recipient's signed-event validation
prevents forgery or silent alteration, and normal retries handle duplicates.

The relay cannot decrypt the inner event unless its operator is separately
an authorized recipient and holds that recipient device's private key.
The outer wrapping has no forward secrecy against later compromise of one
pair's long-term X25519 key. Such a compromise can reveal recorded headers
and controls for that pair; DM text still has its separate Double Ratchet.
Relay copies of V1 events from earlier clients remain readable until evicted
and cannot be retroactively hidden. New clients do not query those V1
conversation IDs from a relay: doing so would disclose the IDs again,
possibly to a different relay. A sender must retry the event through the
opaque mailbox or deliver it directly. Pending V1-only custody may therefore
expire without delivery if its sender never reconnects.

## Routing and delivery

DM and delivery-control conversations are wrapped only for their addressed
device pair. A channel is wrapped only for current members; a removed member
does not get later events. File chunks are wrapped for the explicit file
destination after recipient consent. The recipient pulls its mailboxes with
historically verified peers, decrypts, and runs ordinary local event admission.
This permits an offline sender, channel invitation, receipt, or file to arrive
through the relay while the original sender is stopped. The relay remains a
bounded buffer, not the source of truth: its event-count and 512 MiB ciphertext
budgets evict whole mailboxes, and authors retain their original durable
events for direct retry.
If election changes, the new relay has a different mailbox ID and does not
inherit the old relay's stored blobs. An online sender retries to the newly
elected relay; otherwise delivery waits for that sender or the former relay
to return. Several LAN relays provide a standby, not replicated custody.

## Acceptance and operational limits

Loopback tests cover an offline DM and a channel invitation, assert that the
relay stores only opaque mailbox events, and verify that the recipient can
recover them. Crypto tests cover deterministic deduplication, size padding,
recipient isolation, and signed inner-event validation. A production relay
test rejects an old clear-header upload. The full backend suite includes
delivery receipts, consent-gated files, restart, and private-mode cases.

The current sync engine still exchanges the complete set of IDs for one
mailbox. An in-memory seen-ID cache avoids downloading the same accepted blobs
on every poll. After a recipient restart, it may download the mailbox once
more before local duplicate detection; the cache never suppresses a retry
for an event whose inner history has not been accepted. The relay's
whole-conversation eviction and no-ACK retention model remain. A future
cursor/ACK protocol can reduce bandwidth and storage, but
must preserve durable recipient commit before deletion and handle replay and
crash recovery. Do not claim sender/recipient anonymity or metadata forward
secrecy from this implementation.
