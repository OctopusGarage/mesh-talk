# Account profiles and contact verification

An account profile contains one display name, an optional biography (About), an
avatar and an optional creation timestamp. Editing the ordinary nickname or About
saves the profile locally; signing and synchronization happen automatically.
The conversation header shows the contact's About text in the available space;
its full text is available in the contact's profile. Account ID copying and QR
sharing identify the account. They do not grant verification.

## Creation metadata

New accounts record their creation timestamp when their account key is generated.
The encrypted account keystore installs the key and timestamp together, before
profile initialization. Concurrent creation cannot overwrite an already created
account. The new file framing makes old clients fail authentication rather than
misinterpret metadata as a different secret. Use an updated client for these
account files. A failed startup can retry without losing or replacing that timestamp.
Neither the desktop edit commands nor the ordinary profile edit intent accepts a
creation timestamp. An established timestamp cannot change through profile edits,
rename, avatar changes, login, recovery or device linking.

The timestamp uses the originating device's local clock. Automatic recording and
an immutable application field do not establish independently trusted wall time.
A modified client or an account-key holder can sign false presentation data.
Account IDs derive only from the public key, never from names or timestamps.

Legacy accounts without creation metadata remain unknown. Local user registration,
first login, backup creation and filesystem timestamps are different facts and
must never fill that gap. Old `MTC1` cards may contain manually declared dates;
those dates and their witness receipts never become account creation metadata.
Legacy peer names and biographies remain readable, with the creation date unknown.

Versioned pairing transfers creation metadata independently of message-history
sharing. New initiators require metadata-capable responders: a failed negotiation fails
without silently retrying an older format that would lose creation metadata.
Upgrade the other device before linking if it lacks that support. Legacy request
encodings remain readable by the responder; absent historical metadata stays unknown. Snapshot schema 4 requires a singleton
`AccountCreation` record even when the timestamp is unknown. A snapshot's own
profile must agree with that record. Recovery installs the original metadata
before opening the restored account; a backup's timestamp is never substituted.
Schemas 1–3 remain readable without inventing a creation timestamp.

## Private verification

| Information | Meaning |
| --- | --- |
| Signed profile | This account authenticated these presentation fields. |
| Account creation time | The originating client recorded this local-clock time when generating the account. |
| First observed | This installation first recorded a certified account/device observation at this local time. |
| Device changes | This installation observed different certified devices for the account; multi-device use can be legitimate. |
| Verified contact | You explicitly compared the device safety number through an independent trusted channel. |

A valid profile signature never establishes a person's identity, honesty or
verification status. Profiles never grant verified-contact message pulls.
First-observed time, device history, aliases and verification flags remain private
local state. Legacy observation records without a date remain unknown.

The application has no public introduction, creation-witness, signed JSON import
or export, or separate profile-publication workflow. Frozen legacy signatures and
snapshot tags remain readable for compatibility; they do not restore those user
capabilities or convert private observations into public statements.

## Storage and delivery

`identity/account.rs` and `identity/account_keystore.rs` own creation metadata.
`node/account_profile_data.rs` owns bounded `MTP3` profile signatures and encrypted
current-profile storage. `node/account_profile_io.rs` owns admitted local edits,
legacy migration and authenticated reception. `src-tauri/src/account_profile.rs`
exposes session-bound profile reads and biography edits; the ordinary nickname
uses the existing authentication owner and updates the same profile.

The new profile store retains at most 1,024 current accounts and 32 MiB of encoded
profiles, with bounded journal compaction. Names are capped at 256 UTF-8 bytes,
biographies at 2 KiB and avatars at 128 KiB. Wire decoding is bounded before
deserialization, rejects trailing bytes and rechecks signatures. Equal profile
revisions use the same clear-first avatar order as compatibility profiles, with
signature ordering resolving metadata-only ties; an established creation timestamp cannot
be changed by a later profile. These are resource and consistency rules, not
reputation measures.

The durable profile is the accepted edit intent. Its avatar projection is repaired
on retry or reopening if a secondary write fails. Local acceptance returns without
waiting for network fanout. Host authority is entered before the delivery journal;
profile and avatar publication share the publication lock. A biography edit derives
the current nickname inside that lock, preventing a queued edit from undoing a
concurrent rename.

Each new publication emits `MTP3` followed by its `MTPF1` compatibility avatar. The
avatar event requires the profile event as a parent. Old `MTC1` and `MTPF2` records
remain authenticated and readable; their retired metadata is not reissued. Profile
compaction retains required parents and current leaves. Child-first opaque mailbox
pages defer the child until ordinary parent validation succeeds, without blocking
messages and authenticated recipient receipts in the same mailbox.

The runtime tracks successful profile revisions per present peer. Failed direct
or relay attempts retry accepted events rather than appending duplicates. At most
two profile deliveries run concurrently, with untouched peers ahead of retries.
Existing bounded control deadlines and runtime cancellation apply. A new revision,
including an avatar clear, is eligible without a reconnect; disappearing peers
lose their publication cache and receive the current profile when they return.

See [account and device identity](ARCHITECTURE.md), [module ownership](module-boundaries.md),
[backup recovery](backup-recovery.md) and [contact verification](help/topics/verify-contact.md).
