# Group message reception, ordering and notification patterns

Research date: 2026-10-18. This note examines established protocols and product
behavior. It does not claim that the proposed UX already exists in Mesh-Talk.
The companion code inspection determines the implementation gaps.

## Established patterns

### Separate causal order, arrival order and displayed time

Matrix events reference their parents through `prev_events`, forming a DAG and
establishing a partial order. A sender links the events it knows have no children.
This represents known causal history rather than a universal wall clock.
[Matrix server-server API: PDUs](https://spec.matrix.org/latest/server-server-api/#pdus)

Matrix explicitly documents a difference between `/sync` arrival ordering and
other APIs' partial event ordering. Clients must deduplicate by event ID. Limited
timelines can leave gaps, which clients fill through pagination. Its read-receipt
guidance distinguishes receiving an event from displaying it to the user; private
read receipts can clear notifications without broadcasting a public read receipt.
[Matrix client-server API: syncing](https://spec.matrix.org/latest/client-server-api/#syncing),
[read receipts](https://spec.matrix.org/latest/client-server-api/#read-receipts)

XMPP Message Archive Management preserves an archive's original delivery order
and explicitly warns that timestamps alone do not establish order. Stable archive
IDs, page boundaries and a final query response support resumable retrieval.
Live delivery can overlap archive retrieval and cause duplicates. This is a
server archive model, not a proof of globally identical ordering across independent
servers or disconnected peers.
[XEP-0313 version 1.1.3, sections 3.1, 4 and 6](https://xmpp.org/extensions/attic/xep-0313-1.1.3.html)

**Mesh-Talk recommendation:** retain the signed author timestamp for display;
use validated causal information and a deterministic tie-break for canonical
history, where the existing event model supports it. Record local first receipt
separately for diagnostics and unread accounting. Do not rewrite signed timestamps
to match arrival or imply that concurrent events have a discoverable true order.
Deterministic ordering converges only when devices have the same eligible event
set. Logical clocks express ordering, not elapsed seconds.

### Make delayed delivery distinguishable from fresh activity

XMPP Delayed Delivery explicitly marks messages stored while a recipient was
offline and cached group history. Its `stamp` is the original send time, expressed
in UTC; the extension can also provide a reason for the delay.
[XEP-0203](https://xmpp.org/extensions/xep-0203.html)

Slack provides an unread/catch-up experience separate from the main conversation.
While a user is browsing desktop Unreads, a refresh affordance reveals a new batch
instead of replacing the view immediately. Users can mark messages read or leave
them unread. Notification schedules pause interruptions while messages remain
available for later review.
[Slack unread messages](https://slack.com/intl/en-gb/help/articles/226410907-View-all-your-unread-messages),
[Slack notification schedules](https://slack.com/help/articles/214908388-Pause-your-Slack-notifications)

**Mesh-Talk recommendation:** distinguish live reception, reconnect catch-up,
historical backfill and own-device replication in application events. Preserve
unread discovery for unseen backfill without replaying one sound or desktop banner
per old message. A single summary such as “24 messages received while reconnecting”
can lead to the first unseen item. Fresh mentions may warrant a notification after
the catch-up batch; imported history and duplicates should not. These notification
policies are recommendations, not mandates of XMPP or Matrix.

### Keep receipt, read state and history completeness independent

XMPP Displayed Markers separates display tracking from per-message delivery
receipts. Its design discusses the different granularity and requires a privacy
opt-out for sharing displayed status.
[XEP-0333 version 1.0.0](https://xmpp.org/extensions/attic/xep-0333-1.0.0.html)

**Mesh-Talk recommendation:** use “Saved locally,” “Queued,” “Received by a
recipient device,” and “Read” only when corresponding evidence exists. File-card
acceptance and file-byte availability require separate status. Do not infer read
state from transport receipt, an open-but-background window, or account sync.

A simple timestamp watermark is insufficient for unread state when late messages
can be inserted before it. Track seen message identities or a causal frontier with
explicit exceptions; reconcile own-account read state separately from public
receipts. A late unseen message needs a discoverable unread item even if its
author timestamp is older than the previous read marker.

### Preserve reading position and provide calm status

W3C guidance requires displayed status messages to be programmatically available
without moving focus. `role="status"` offers polite announcements. The guidance
also cautions against excessive live-region chatter.
[WCAG status messages](https://www.w3.org/WAI/WCAG21/Understanding/status-messages.html),
[ARIA22](https://www.w3.org/WAI/WCAG21/Techniques/aria/ARIA22)

**Mesh-Talk recommendation:** anchor the viewport to message ID plus offset when
inserting older events; scroll automatically only when the reader is already at
the live edge. Show a “New messages” or “Earlier messages received” action when
needed. Batch screen-reader announcements for catch-up and preserve focus.

Connectivity status should distinguish “Reconnecting,” “Catching up,” “Waiting
for a peer,” and “Known history synchronized.” In a serverless network, an exchange
with one reachable peer cannot prove that every offline member has no additional
events. Display explicit known gaps or unavailable retention ranges when supported
by evidence; do not infer an exact missing-message count from a time interval.

## Current Mesh-Talk implementation audit

This is static source inspection of branch `opt/optimization-20261018` at
`7fc5c61`, not a new runtime or visual validation result.

| Area | Current implementation | UX implication |
| --- | --- | --- |
| Event replication | `eventlog/store.rs::events` orders by `(lamport, id)`; `prepare` builds parent references and advances the logical clock. The store explicitly does not validate peer Lamport monotonicity. | Reuse causal metadata, but validate it or use a validated topological traversal before making causal display guarantees. |
| Group timeline | `node/queries.rs::channel_history` delegates to `history`, which sorts messages and file cards by `(wall_clock, id)`. | The same retained event set converges, but clock skew can invert an observed question/reply relationship. Late events can insert into older history. |
| Reception classification | `node/serving.rs::process_channel` emits newly decryptable messages. `src-tauri/src/events.rs::ChannelMessageEvent` exposes no event ID, author timestamp, first-receipt timestamp or catch-up classification. | The host/frontend cannot reliably distinguish a live message from a newly decrypted old message using this event contract. |
| Notifications | `events.rs::notification_payload` respects settings, preview privacy and active conversation focus; `emit_channel_message` invokes notification handling per message. | Useful privacy/focus protection exists, but this path has no explicit backlog summary policy. Native OS behavior may further coalesce notifications; that is not an application guarantee. |
| Unread | `frontend/src/store/chat.ts::get_handleChannel` bumps an inactive/unfocused conversation counter per callback. `markActiveRead` clears a focused active conversation without checking individual visible messages. | A counter is not evidence of which events were read. Late insertions above the viewport and duplicate signals need identity-based reconciliation. |
| Sibling imports | The owner-scoped `history_revision` poll reloads active history without notification callbacks. | This avoids replay notifications, but is not a complete mechanism to surface newly imported unseen events in inactive conversations. |
| Reading position | `ConversationView.tsx` uses stable row keys, a virtual list, conditional bottom following and a jump action; `useConversationViewport.ts` restores pixel/index positions. | Useful foundations exist. There is no explicit application-level message-ID-plus-offset anchoring contract for arbitrary middle insertions. Actual layout behavior still needs browser tests. |
| Delivery evidence | `DeliveryFooter.tsx` distinguishes DM receipts, personal local saves and group custody confirmations; `node/channel_custody.rs` documents historical possession rather than read/delivery. | Preserve these distinctions. Group replica counts are not proof that every member has received or read a message. |
| Time display | `frontend/src/lib/format.ts` formats author time in the local timezone. | Different local clock labels or date boundaries can represent the same original timestamp. Timezone differences should not alter canonical ordering. |

## Recommended sequence

1. Define reception identity, first-receipt metadata, source classification and
   batch boundaries, keeping local receipt state outside signed author events.
2. Design validated causal timeline ordering for group messages and file cards,
   including records imported through account history. Do not simply expose the
   current unvalidated Lamport sort or impose a global sequence server.
3. Build unread reconciliation from stable event identities and explicit viewed
   state, with a bounded persistence/compaction strategy. Keep own-account read
   synchronization optional and distinct from public read receipts.
4. Add message-anchored viewport preservation, late-arrival navigation and batched
   notification/accessibility announcements.
5. Expose conversation-scoped recovery status with honest knowledge boundaries,
   then test partitions, reconnects, clock skew and simultaneous live/backfill traffic.

Application behavior has not been modified by this research pass.

## Suggested acceptance scenarios

1. Deliver the same group event set in different orders; final canonical histories
   agree, duplicates do not change unread counts, and author timestamps survive.
2. Backfill older events while the user reads the middle of history; visible
   message and keyboard focus remain stable.
3. Recover a large backlog containing mentions and live concurrent traffic;
   preserve unread discovery without a notification storm.
4. Receive a late event older than the read marker; it remains discoverable until
   actually viewed or explicitly marked read.
5. Switch reachability between LANs, relays and offline members; UI communicates
   progress without claiming global completeness.
6. Receive a file card before bytes; show availability and transfer state separately.

These scenarios are a proposed test target, not results from executed tests.

The implementation following this audit is documented in
[group conversation reception](../group-conversation-reception.md). The source
findings above describe the pre-change baseline, not the current implementation.
