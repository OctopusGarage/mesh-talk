# Message Favorites

Favorites is an account-owned collection of useful messages and personal notes.
It is separate from **Message Yourself**, the self-account conversation, and from
contact pins/aliases. [User instructions](help/topics/favorites.md) explain the UI.

## Collection and metadata

The message menu offers **Add to Favorites** and **Select messages**. Select up
to 100 accepted messages from one direct or group conversation and collect them
as one item. The core resolves exact canonical message IDs and stores their
snapshots in original conversation order; UI text is never source authority.
Pending, failed, recalled, unavailable, explicitly expired and protected sources
are rejected. Protected text and images cannot be collected or viewed through
Favorites; see [protected-message privacy controls](privacy-controls.md#protected-messages).
A multi-message save is atomic: it never silently omits a selected source.

Each item has an optional title (200 Unicode scalar values), overall description
(4,000) and a note for each message (1,000). Those fields can be edited later and
are searched along with content, sender/conversation labels and filenames.
Original saved content is read-only. Labels are presentation context, not verified
identity claims. Search queries are bounded to 200 characters; pages contain at
most 100 items and have an aggregate byte budget. The UI requests 20 at a time.
Each stored snapshot is bounded to 256 KiB. Oversized content is rejected rather
than truncated. Repeating the same exact selection reuses the existing item and
preserves its metadata; overlapping selections can capture distinct context.

## Lifetime and files

The core owns an encrypted account-specific journal independent of ordinary chat
history. Clearing or pruning source history does not delete a favorite. Removing
a favorite does not alter its source messages. Explicit message expiry still
withholds that member's content and file access; other messages and personal notes
remain. `View original` uses the exact namespace, conversation and message ID;
unavailable originals never fall back to matching text or timestamps.

Already available authorized attachment bytes up to 64 MiB per file can be copied
to an independent encrypted favorites media store. Unavailable or larger files
retain their file cards; a card does not promise retained bytes. Collection does
not grant transfer consent. File access requires a live unexpired favorite
reference; removing the last reference permits cleanup. Account-data deletion
removes the collection with the account's other local encrypted data.

## Backup and recovery

Manual local-file and Google Drive backups include Favorites, titles,
descriptions, message notes and independently retained attachment bytes, even
after source history has been cleared. Both destinations use the same encrypted
recovery archive; Google sign-in does not replace its separate recovery key.

Snapshot schema 5 carries logical collection records, including durable removal
tombstones. Restore retains the account identity, generates a fresh device, and
re-encrypts the collection and retained files under the new local password.
Expired message content and file access stay unavailable, and removal records
continue to reject stale edits within the same generation. Retained files remain
subject to live references and the 64 MiB per-file bound; missing bytes stay
card-only and are counted separately in the preview. Ordinary media and retained
Favorite files share the archive's aggregate media budget. Older snapshots remain
readable but contain no Favorites. See [backup coverage](backup-recovery.md).

## Synchronization

Certified devices on the same account exchange a separately versioned, optional
Favorites request through the existing account-sync owner. Existing V1/V2 history
frame tags/encodings are preserved. Unknown optional requests on older devices
cannot substitute for successful Favorites sync or break supported history sync.
Pages are bounded to eight records, with 16 KiB chunks and a 256 KiB record limit.
Whole-exchange deadlines reuse the existing history/file budgets; per-chunk
timeouts do not replace them. Unchanged journals use a digest response, and file
transfers run separately through the existing bounded fair attachment workers.

Both devices must allow **Message history** to exchange collection content,
including title, description, message notes and removal records. Both must also
allow **Attachments** to exchange retained file bytes. Disabling synchronization
preserves the local collection. Offline edits reconcile when a permitted,
compatible source becomes reachable; there is no vendor-operated archive.

Whole metadata records use a deterministic revision/device ordering. Editors
validate both revision and device identity. Removal wins offline metadata edits
within one collection generation; only an explicit later re-save creates the next
generation. Durable removal tombstones prevent an older offline copy
from resurrecting a removed favorite. Only an explicit later re-save supersedes a
removal. Tombstones remain until a separately validated reclamation policy exists.

## Ownership and acceptance

`core/node/favorites.rs` owns canonical capture, encrypted persistence, queries,
retained-file authorization and metadata transitions. Its recovery adapter owns
logical collection encoding and validation; `core/node/recovery.rs` and
`core/recovery/snapshot.rs` own archive streaming, validation and staged install.
`core/node/account_sync/favorites.rs`
owns its optional sibling exchange. Here `core/` abbreviates
`crates/mesh-talk-core/src/`. Desktop `src-tauri/src/message_favorites.rs` adapts
commands under exact runtime/session admission through durable IO and plaintext
observation. Frontend typed IPC and `store/messageFavorites.ts` own reactive
queries; `features/chat/favorites/` owns selection, editing and browsing.
Selections and drafts retire on conversation/session replacement; late search or
mutation completions cannot disclose another owner's state.

Focused journal and loopback tests establish persistence, atomicity, stale edits,
replay, policy intersections and unrelated-account rejection. Registered IPC tests
cover queued session replacement and app lock. Browser scenarios cover collection,
notes, search/edit/remove, exact navigation, keyboard and narrow windows. These
checks complement each other: mocked browser IPC does not certify native webviews
or real cross-machine sync. Owning `validation.json` files select these checks.
