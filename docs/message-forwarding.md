# Message forwarding

Forwarding creates new messages from ordinary retained conversation content.
The forwarding account is the authenticated sender. Historical sender names and
timestamps inside a merged record are display snapshots supplied by that account;
they do not authenticate the original authors again.

## Interaction

Use a message's **Forward…** action for a single message, or **Select messages**
for an ordered selection. Selection is scoped to the source conversation and
current desktop runtime. Virtualized rows and loading older history do not discard
selected records. Cancelling leaves the conversation's unfinished draft intact.
Favorites and forwarding share selection mechanics and keep independent eligibility
and persistence rules.

Selections are limited to 100 messages and five destinations per operation.
Individual attachments have a 48 MiB preparation limit; merged archives have a
64 MiB encoded limit and a 48 MiB aggregate attachment budget.

Choose individual forwarding to send separate ordinary messages, or merged
forwarding to send one read-only chat record. Search and select destination
accounts or writable joined groups, review the source summary and targets, then
confirm. An accepted send is queued durably; it is distinct from recipient delivery
or reading. Partial results identify failed work; retry uses the same operation
identity and does not send accepted work again.

## Source admission and lifecycle

The core resolves exact source conversation/message references in canonical
history order. Renderer snapshots are previews, not send authority. Protected,
recalled, expired, failed, pending and system records cannot be forwarded. Missing
attachment data must produce an explicit failure; it must not disappear from a
merged record. Eligibility, current session authority and destination write access
are checked at acceptance as well as inspection.

A forwarded message has independent destination identity and lifecycle. Reply IDs
from another conversation are not used as destination reply targets. Source recall
after acceptance does not remotely remove an already forwarded copy. Explicit
expiry must never be extended or removed by forwarding. Individual text preserves
the original absolute expiry. Expiring attachments and merged selections containing
expiring messages are rejected until those paths can enforce the same deadline.

Attachments are resolved through the source's retained authorization, prepared
with independent file identity/key material and sent through the normal encrypted
attachment/consent lifecycle. Recipients do not need source conversation membership.
No source file keys, local paths or temporary Blob URLs belong in the archive.

Group destinations use the account's Owner or Member role, including certified
reference-only siblings. An already executable local device retains sequential
text acceptance through the existing send owner. Logical siblings delegate text,
and all group individual files and merged records enter the existing account-group
operation journal and execute through its selected eligible physical device. Reference metadata does not install membership or sender keys.
A group leg remains pending until its verified committed result supplies the
original public message identity. Unknown or unauthorized group legs fail before
file restaging or network admission; other accepted or pending legs remain intact.
The canonical source policy and current runtime authority are checked again at
journal admission, including after asynchronous file preparation.

## Merged record format

Merged records travel as ordinary encrypted `.mtchat` attachments. Capable clients
validate a versioned record before showing its read-only contents; other clients
can receive an ordinary file. Rich metadata is available after validated bytes
arrive; generic file announcements must not imply that content has been decoded.
The container's title and historical names are untrusted display text.

Parsers bound record count, total encoded/decoded bytes, attachment bytes and text.
Nested record containers are rejected. Attachments use contained bytes, not local
paths or remote fetch instructions. Renderers display text and owned media URLs;
they do not execute supplied HTML or scripts. Object URLs retire when the viewer,
owner or runtime retires.

## Reliability and verification

A stable operation identity binds the exact source, destinations and mode. A
changed request cannot reuse that identity. Per-target/item acceptance identities
and persisted progress distinguish accepted work from work eligible for retry.
Dispatch is bounded independently of the frontend's ordinary send-intent capacity.
Unaccepted operations resume through explicit review/retry after restart, rather
than starting sends without user confirmation. The local forwarding-operation log
is device-local and is not included in account synchronization or recovery archives;
accepted messages continue through the existing message delivery/recovery owners.
Operation IDs contain their creation timestamp; operations older than seven days
are rejected before retry. Recent operation fingerprints are retained within this
window, with at most 128 full operation records and 4096 fingerprints; only completed
full records are retired. If acceptance started before a crash but its exact
request identity can no longer be recovered, the item reports uncertain acceptance
and is not automatically resent. Inspect destination history before starting a
new operation for that content.

For group legs, the existing durable batch/item identity derives one origin-bound
account-group operation ID. Retry observes that same journal entry instead of
restaging or submitting a second intent. The forwarding record layout is unchanged:
`pending` carries no message ID; `accepted` carries the verified original event ID.
If an original pending intent cannot be found after journal loss or an origin
change, the item reports uncertain acceptance and requires review rather than
silently binding another origin.

Within a batch, same-target items retain canonical source order in the existing
progress record. The forwarding owner withholds a later group's signed operation
request, and local execution, until earlier admitted items reach a terminal result.
Committed, cancelled or rejected predecessors release the next item; uncertain
acceptance remains blocked. Other targets and unrelated operations keep their
existing scheduling. This fence survives restart without a new wire or stored field.

Batch listing projects retained journal status without admitting work, advancing
query retries or rereading source content. Source recall after admission does not
prevent observing or replaying the already admitted result. The existing forwarding
writer reconciles verified terminal receipts before applying its batch-retirement
budget, so committed group work does not remain stored as unfinished indefinitely.

Focused tests cover selection, source policy, canonical order, request conflicts,
partial retries, restart recovery, archive parsing and owner retirement. Encrypted
loopback coverage must check text and independent attachment retrieval across
account/group destinations. Browser checks exercise menus, selection, target
search, confirmation, cancellation, draft preservation and partial failure.
Native desktop interactions and real offline/network topology acceptance remain
separate evidence from browser IPC substitutes; see
[validation ownership](testing-feedback-loop.md#validation-stages-and-agent-responsibilities).
