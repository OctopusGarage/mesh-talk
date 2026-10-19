# Group conversation reception

Group members can receive messages at different times through direct links,
member recovery or post offices. A device can remain useful offline without
pretending that its history contains everything another offline member knows.

## Reading a conversation

Messages retain the original sending device's timestamp. The displayed time uses
the viewing device's timezone, so two devices can show different clock labels for
the same instant. A message's position is not determined by when this device
downloaded it.

Group history follows retained, verified event-parent relationships, including
relationships passing through membership and other control events. Concurrent
ready events use the original timestamp and event ID as deterministic tie-breaks.
Logical clock values alone are not trusted as evidence of causality. Both group
processing and presentation traverse the verified graph; processing retains its
existing logical-clock tie-break among concurrent ready events.

Devices with the same retained graph and archive records converge on the same
presentation order. Plaintext-only history imported from older account archives
does not contain signed ancestry. Those records use a timestamp/ID fallback
until the original event graph is available. Their ancestry is not fabricated
from an unverified reply reference. Signed timestamps are never rewritten to
make clocks appear synchronized.

While reading older content, the viewport preserves a visible message identity
and its offset when history is inserted. The conversation follows new output
automatically only when already at the bottom. **New messages** and **Earlier
messages received** actions navigate to unseen messages. If a selected message
is outside the initial view, navigation requests a larger bounded history window.
If it still cannot be opened, an explicit status and mark-read action replace a
silent failed jump.

## Unread messages and notifications

Group reception events carry stable event identity, authored time, local receipt
observation, own-device identity and recovery batch metadata. Local receipt time
means the local observation of newly decrypted content, retained through local
persistence retries; historical imports do not invent an original local receipt
time.

An explicit member-recovery or mailbox processing pass is catch-up activity.
Multiple newly decrypted messages in one pass are also treated as a catch-up
batch. An ordinary inbound singleton is live activity for presentation purposes;
this classification does not prove that the author just composed it. Sender
clock age is not used to guess network delay.

Native catch-up alerts summarize unique external messages in each processing
batch. Own-device/history replication is silent. Notification settings, focused
conversation suppression, preview privacy and application lock rules still apply
to summaries. Separate recovery passes can produce separate summaries; the app
does not promise a single alert for an arbitrarily long disconnection.

Unread tracking uses identities rather than a timestamp watermark. Receiving a
late message before a previously read timestamp does not mark it read. Visible
message content in a focused, visible conversation can be marked viewed; opening
or focusing a conversation does not clear offscreen tracked identities. An
explicit mark-read action can clear the conversation. Older events without IDs
retain legacy counter behavior without clearing tracked unread exceptions.

The virtual message log does not announce every inserted row. A polite status
region batches newly discovered unread identities in fixed windows; duplicates
and read-count decreases do not deliberately replay announcements. Recovered
history is not animated as a new live message.

## Recovery and availability

Active accepted groups with other members can expose local recovery observations:
waiting for a reachable member, exchanging retained history, or a completed peer
check with an observation time. Self-only, pending, archived and dissolved groups
do not show a misleading recovery wait. A completed check is not proof that all
offline devices or group members have no additional messages.

Group custody indicators report historical retained copies, not universal delivery
or reading. A file card can appear before the file bytes are available. Receiving
the card, retaining chunks, downloading the contents and reading the conversation
remain separate facts.

## Local storage recovery

A failed plaintext save pauses further group decryption instead of discarding a
message whose receive key has already been consumed. Exact pending plaintext is
retained with a 64-message, 8 MiB estimated-allocation budget, then retried
with capped backoff even
without a new peer connection. The uncommitted suffix stays ordered and recovery
batch completion waits for persistence or an explicit suppression/expiry outcome.
Successful rows are published in order; a separate metadata-only terminal
completes the batch without retaining or republishing message plaintext. Desktop
consumers handle this terminal before message deduplication or unread counting,
and legacy message callbacks skip it. Convenience `NodeRuntime` constructors
preserve the legacy message-only callback contract; configured
`RuntimeEvents.on_channel` callbacks and raw `Node` channel receivers also receive
controls and must branch on `reception_only`. The desktop host uses this explicit
reception-aware contract.
A replay cursor traverses the validated graph once per processing pass rather
than rebuilding its order for each bounded chunk. Channel file projection waits
behind this local processing boundary so it cannot
advance receive state ahead of paused message/control processing.

Local clear and retention suppress matching retained content identities before
erasing projections, including content not yet projected. Restart repair can
rebuild missing eligible projections from retained signed events and key
distributions without replaying notifications or resurrecting suppressed rows.
External expiry-wrapped records are conservatively excluded from startup repair.
The in-memory retry queue is not an atomic crash-safe cryptographic journal;
restart reconstruction depends on retained evidence, and callbacks do not have
an exactly-once guarantee across arbitrary crashes or power loss.

## Resource and compatibility boundaries

- Exact frontend reception identities are bounded to 64 conversations, 1,024
  identities per conversation and 8,192 identities overall. Unread entries have
  priority over older seen entries. Evicted unread identities become aggregate
  overflow counts, with a visible review/explicit mark-read action. Focus does
  not clear this overflow. Exact deduplication and navigation beyond the retained
  identity window are limited; overflow is not an exact unlimited message index.
- Persistence is scoped to host and mesh account, validates stored entries, saves
  no message text and caps ledger JSON snapshots at 3,000,000 characters. Counter
  and overflow snapshots have separate 1,000,000-character limits. Overflow keeps
  at most 4,096 conversation counts, capped at 1,000,000 each. Existing counter
  snapshots remain readable. Private-mode/quota failures preserve session operation.
- Inactive history reconciliation processes at most eight snapshots per roster
  round, with up to 500 rows per snapshot and a bounded 4,096-conversation work and
  baseline registry. Failed reads retry without starving later queued work.
  Initial presentation covers 200 rows; full snapshot coverage is tracked
  separately so expanding to 500 does not turn old history into fresh unread.
  Scoped coverage evidence survives lock/unlock without retaining message text.
  Subsequent observed revisions discover newly imported external identities
  within retained snapshot coverage. This is not pagination over every retained
  historical row: imports outside that window do not receive exact unread
  indexing through the snapshot scanner. Evicted identity snapshots are
  rebaselined quietly rather than replaying an old notification backlog.
- Native deduplication retains at most 4,096 event identities and 128 batch
  records per active owner. Durable core projections prevent ordinary restart
  replay from publishing already stored channel messages again.
- Reception metadata is additive to desktop events; signed network formats and
  existing account archive decoding remain compatible. Old desktop events can
  still be consumed, with their less precise legacy unread behavior.

## Validation

The implementation was checked with focused frontend unit tests, browser E2E,
real core loopback tests and source-based AI evaluations. Browser tests use the
Tauri IPC mock; they do not replace native transport tests.

- The focused frontend run passed 155 tests across eleven files, covering unread
  retention, stale owners, lock/unlock, viewport insertion, recovery status,
  delivery wording and locale parity.
- The full default-build Chromium run selected 349 tests: 341 passed, four
  failed and four were intentionally skipped. The skipped cases require the lite
  build, opt-in site capture or physical Mac/Windows media environments. The four
  failures exposed oversized-row auto-follow/restoration bugs and asynchronous
  pack setup races. Corrections use the virtualizer's scroll API, protect saved
  offsets during initial measurement, update jump visibility as rows grow and
  explicitly await required pack setup. Regression tests cover nonzero reading
  offsets inside oversized messages.
- The final serial acceptance run passed all 49 tests in `chat.spec.ts`,
  `group-reception.spec.ts`, `packs.spec.ts` and `avatar-gallery.spec.ts`, with one
  worker and no retries. It includes every original failure above, nonzero
  reading-position restoration and all four group reception cases.
- `make e2e` passed all 12 real multi-process scenarios plus the separate 110 MiB
  large-folder regression. These cover routed and cross-LAN discovery, direct and
  group messages, byte-for-byte file transfer, restart history and offline custody.
- Frontend production build, both TypeScript configurations, ESLint and Prettier
  checks passed. Workspace Clippy passed with `--all-targets -- -D warnings`.
- The focused core run passed all 13 tests: causal ordering, backward logical
  clocks, bounded replay, persistence retry ordering, erase/expiry terminal
  privacy, restart suppression, and real loopback text recovery and file cards.
  A broader run from the same final test binary selected `channel`,
  `account_sync`, `privacy_runtime` and `group_chain`: 204 passed, none failed,
  and two declared scale rehearsals were ignored.
- The CLI file-save suite passed all six tests, and the standalone native
  reception-ledger suite passed all six tests, including metadata-only terminal
  completion without counting a suppressed message identity.
- A core regression replayed 4,097 messages through 65 bounded chunks using one
  ordered cursor. This checks that chunking does not repeatedly rebuild the graph;
  it is not a network throughput benchmark.
- All 27 source-based AI cases passed using the configured Codex command provider:
  ten default, six group reception and eleven linked-device cases. The group
  suite was refreshed against the viewport corrections and scored 0.96 to 0.98,
  with no required improvements. Deterministic `make eval-smoke` and all nine AI
  harness tests also passed. Model judgments supplement executable regressions;
  supplied source context defines the extent of each model review.

The focused core command was:

```bash
cargo test -p mesh-talk-core --lib --features fast-test-kdf -- \
  group_reception \
  channel_messages_are_forward_secret_and_history_comes_from_stores \
  channel_member_recovers_missed_message_from_peer \
  saved_channel_file_remains_downloadable_from_member_after_author_disconnects
```

Full workspace tests, the complete health gate, cross-platform PR preflight and
hosted CI were not run for this change. Local passing checks do not establish the
status of `origin/main` CI.

The [research note](research/group-message-reception-ux.md) records the prior
audit and primary-source rationale. The [network reference](network-design-reference.md)
owns transport authority and resource limits.
