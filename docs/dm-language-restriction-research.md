# Direct-message language restrictions: feasibility and proposed scope

Implementation follow-up: [incoming DM language](dm-language-restrictions.md) owns the implemented behavior and limits. The proposal below records the preceding investigation.

Research date: 2026-10-19. The requester confirmed that the restriction applies to
direct messages **to their account**, excluded translation, and expanded the rule
to selectable languages with a default tolerance for mixed-language text. This is
a revised proposal based on source investigation; application behavior has not
been changed.

## Recommended scope

Add an opt-in account setting: **Incoming message language**. Default: unrestricted.
When enabled, the owner chooses one primary language, such as English, Chinese,
Japanese, or Spanish. Offer the languages that the selected local detector can
actually classify and that have passed representative validation. Interface
translations and detector language support are different capabilities; do not
promise every UI locale or dialect is distinguishable by the detector.

Use a default minimum primary-language proportion of **80%**, allowing up to
**20% other-language content** per message. These are product defaults, not an
empirically established universal optimum. Do not add a percentage slider in the
initial UI: show the default in supporting text and validate its usability before
release. Changing the primary language uses the same default tolerance.

When enabled, updated clients block text exceeding the tolerance before sending
to this account and preserve the draft with an actionable error. The receiving
endpoint also checks incoming text so clients that ignore the preference cannot
bypass it in the normal chat view. Detection runs locally, without a network
service.

The setting applies to all incoming direct-message conversations for that
account. It does not constrain that user's outgoing messages, unrelated
recipients, groups, or interface language. Images, files, stickers, and calls are
outside the text-language rule. Existing history is not retroactively rewritten.

Translation buttons, Google APIs, external translation links, OAuth, API keys,
billing, and quota handling are excluded from this feature.

## Alternatives and recommendation

| Approach                                                                                  | Tradeoff                                                                                                                      |
| ----------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Sender-side UI check only                                                                 | Smallest change, but legacy clients, CLI calls, and modified clients can bypass it. Suitable only for a best-effort reminder. |
| Shared local validation, authenticated recipient preference, and receiving-side filtering | More work at the protocol and history boundaries, but supports the recipient-owned restriction. Recommended.                  |

No classifier can perfectly identify every language span in arbitrary text. The
feature can enforce a documented approximate proportion, with occasional mistakes.

## Proposed user experience

1. Enable the setting and choose a primary language. Updated senders see, for
   example: **Please write mainly in English. Up to 20% other languages is allowed.**
2. The sender can draft freely. On Send, locally classify the text and check the
   latest verified recipient preference before creating a durable send intent.
3. If disallowed, retain the draft and show: **Please write at least 80% of this
   message in English.** Substitute the selected language. If classification is
   inconclusive, suggest a longer sentence in that language. There is no
   send-anyway action.
4. Permit mixed-language text within the tolerance and explicitly allowed neutral
   content: numbers, emoji, URL-only text, bounded code spans, and common short
   acknowledgements such as `OK`. Do not exempt a whole message because it contains
   a URL or code fence.
5. Independently filter newly received noncompliant messages from normal chat,
   previews, notifications, and search. Use a content-free placeholder such as
   **Message hidden by your language setting** if the event needs a visible
   position. Do not introduce a separate quarantine-management UI in the first
   version.

Short names, identifiers, typos, quotations, transliteration, and mixed-language
text need examples in the decision policy before implementation. Very short
acknowledgements use explicit exceptions rather than pretending that an 80/20
split can always be established. Other wholly ambiguous prose is blocked with a
request to rephrase; this may reject legitimate messages.

## Proportion calculation

The 80% threshold refers to **text content**, not the detector's confidence that
the whole message belongs to one language. Whole-message detection alone cannot
estimate mixed-language proportions.

1. Identify bounded neutral spans (URLs, known mentions/identifiers, code) without
   exempting surrounding prose. Ignore whitespace, punctuation, numbers, and emoji.
2. Segment the remaining prose into language-labelled spans using local detection.
   Script is a hint, not proof: English and Spanish share Latin letters, while
   Chinese and Japanese can share Han characters.
3. Weight spans by the count of visible letter/text graphemes, rather than UTF-8
   bytes or a whitespace word count. Combining marks must not inflate the count.
4. Let `T` be the weight assigned to the selected language and `A` the total
   non-neutral weight. Count unclassified prose toward `A`, not `T`. Allow when
   `100 * T >= 80 * A`. If `A` is zero, allow neutral-only content. Apply explicit
   short-acknowledgement exceptions before this calculation.
5. Use the same normalization, segmentation, classifier version, and arithmetic
   on sending and receiving paths. Make the decision on the original draft's
   content after agreed normalization; do not replace or rewrite its text.

Illustrative labelled-content weights: 85 target + 15 other passes; 80 + 20
passes; 79 + 21 fails. Neutral content cannot dilute the other-language share.
The rule is per message, not an average across conversation history.

This is a character-based approximation, not semantic word equivalence: one Han
character and one Latin letter do not carry equal meaning. Benchmark real mixed
sentences in both language directions before claiming consistent usefulness.
Do not publish a measured percentage as exact linguistic truth.

## Detection and ownership

Keep one bounded classification decision in the Rust core, consumed by sender
acceptance and receiver projection. The UI provides immediate feedback and
preserves drafts; the core owns the final check. Local classification keeps the
feature available offline and avoids uploading private text.

ASCII or script checks alone do not establish a language. A detector candidate
such as Lingua needs evaluation on representative chat data before selecting a
model or confidence-handling rule. Its documentation describes short-text ambiguity and
experimental mixed-language detection; upstream accuracy claims do not establish
accuracy in Mesh-Talk. See [Lingua documentation](https://github.com/pemistahl/lingua-rs).

## Current repository behavior and extension points

The investigation used local source at commit `762525d` and the maintained
[architecture](ARCHITECTURE.md), [network contracts](network-design-reference.md),
and [module ownership](module-boundaries.md). There is no `.codegraph/` directory
in this checkout, so source was located directly.

| Area                       | Current source evidence                                                                                                                                                                                                                                                    | Implication                                                                                                                |
| -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| Interface language         | [`frontend/src/lib/i18n.ts`](../frontend/src/lib/i18n.ts) supports English, Spanish, Japanese, Simplified/Traditional Chinese, and Cantonese.                                                                                                                              | Interface language and permitted message language need separate controls.                                                  |
| Composer                   | [`Composer.tsx`](../frontend/src/features/chat/Composer.tsx), `send`, trims text, calls synchronous `onSend`, and clears the draft on acceptance.                                                                                                                          | Asynchronous classification must finish before acceptance; rejection must preserve the draft.                              |
| Send ownership             | [`ConversationView.tsx`](../frontend/src/features/chat/ConversationView.tsx) calls `admitText`; [`outgoingIntent.ts`](../frontend/src/store/outgoingIntent.ts), `dispatchIntent`, sends account text through `chat.enqueueText`.                                           | Put final validation before durable enqueue; cover retries and programmatic sends as well as the button.                   |
| Desktop boundary           | [`owner_commands.rs`](../src-tauri/src/owner_commands.rs), `owner_enqueue_text`, captures an owner/runtime lease and calls `enqueue_to_account_expiring_if`. [`messaging.rs`](../src-tauri/src/chat_commands/messaging.rs) also exposes legacy send commands.              | Keep account isolation; updating only the legacy `send_dm` command would miss the main UI path.                            |
| Core send                  | [`node/dm.rs`](../crates/mesh-talk-core/src/node/dm.rs) owns device/account sends and immutable delivery acceptance.                                                                                                                                                       | Reuse a shared validation decision for desktop and CLI/core consumers. Preserve already accepted immutable outbox events.  |
| Identity metadata          | [`profile.rs`](../crates/mesh-talk-core/src/node/profile.rs) has an account-signed, avatar-only `MTPF1` payload; [`profile_io.rs`](../crates/mesh-talk-core/src/node/profile_io.rs) authenticates and distributes it.                                                      | This establishes an identity-bound propagation pattern, not an existing language-policy field.                             |
| Receiving                  | [`delivery_runtime.rs`](../crates/mesh-talk-core/src/node/delivery_runtime.rs), `accept_dm_event`, authenticates, decrypts, persists, and prepares receipts. [`serving.rs`](../crates/mesh-talk-core/src/node/serving.rs), `emit_new_messages`, processes received events. | Validate plaintext at the endpoint. Relays cannot classify encrypted chat text. Preserve ratchet and receipt transactions. |
| History and other surfaces | [`queries.rs`](../crates/mesh-talk-core/src/node/queries.rs), `account_history`, reconstructs history separately; [`search.rs`](../crates/mesh-talk-core/src/node/search.rs) and host [`events.rs`](../src-tauri/src/events.rs) own additional exposure paths.             | Hiding only a live message bubble leaks blocked text through history, search, previews, or notifications.                  |

The existing message framing is deliberately versioned and positional:
[`message.rs`](../crates/mesh-talk-core/src/node/message.rs) preserves `MTB1` text
and uses `MTB2` for extra fields. Profile decoding rejects trailing bytes. Do not
append a language field to a bincode struct and assume JSON-style defaults make
it compatible. Policy propagation requires an explicit wire evolution decision and old-client tests.

## Protocol and receiving boundaries

- Persist whether the restriction is enabled, the selected language, and the
  minimum target-language percentage. Publish an authenticated account-level
  preference using existing account/device trust. Cache verified peer preferences
  for offline sends. Define revision ordering, disabling updates, and linked-device
  convergence. Unknown preference is not evidence of a verified disabled rule.
- Do not mutate the strict avatar-only `MTPF1` layout or change positional bincode
  structs without an explicit compatibility strategy. Supporting policy metadata
  does not require changing ordinary text message bodies.
- Recheck before durable enqueue and cover programmatic, legacy, and retry paths.
  Never rewrite accepted immutable events. A queued message arriving after mode
  activation is checked under the receiver's current known rule.
- Apply receiver classification after authentication/decryption and consistently
  across live messages, history, search, previews, and notifications. Bound work
  and cache the result by stable message identity and policy/classifier revision.
  Preserve ciphertext and ratchet/recovery semantics rather than silently corrupting
  or deleting signed events. Hidden content remains subject to local retention.
- Preserve the current meaning of Delivered: durable endpoint receipt, not proof
  of display or language approval. Filtering must not cause infinite retries or
  false receipts. A new remote rejection status is outside the initial scope.
- Old or modified clients can transmit ciphertext despite the restriction.
  Receiver filtering protects normal presentation; it cannot prevent every
  network byte or temporary stored copy. Relays cannot inspect encrypted text.
- Linked devices learn policy changes when synchronized. No immediate global
  guarantee is possible while devices are disconnected. Previously accepted
  history remains unchanged; newly filtered messages are not automatically
  revealed when the setting changes unless that behavior is explicitly chosen.

These constraints follow from the current source and the
[network architecture baseline](network-design-reference.md), particularly its
endpoint-owned delivery truth and immutable event requirements.

## Focused implementation and validation

Proposed work, pending agreement on the design:

1. Evaluate a local detector and span-level classification, settle supported
   languages and neutral/ambiguous input examples, and evaluate the 80/20 default.
   Measure false blocks, false passes, latency, package size, and memory on target
   platforms. A single whole-message language label is insufficient.
2. Add the authenticated preference and persistence with backward-compatible
   protocol handling, default off, and linked-device propagation.
3. Add shared validation, sender notice/error, and consistent receiver filtering.
4. Verify focused Rust and frontend behavior. Networking changes require a
   two-node loopback assertion under the repository guidelines.

Cover selectable target languages, same-script pairs, CJK, mixed language,
short/unknown input, Unicode punctuation/combining marks, names, code/URLs, emoji,
empty/long inputs, and draft preservation. Verify exact 80/20 boundaries, exclusion
of neutral padding, unclassified spans, and target-language switches. Cover
toggling, account isolation, forged/stale preferences, offline
queueing, legacy/noncompliant clients, linked devices, restart, and all content
projection surfaces. Preserve delivery receipts, retries, and immutable event IDs.

## Current status

The confirmed product direction is selectable primary-language restrictions with
some mixed-language tolerance, without translation. The proposed default is 80%
selected-language content and up to 20% other-language content. The recommended
design is an account toggle and language selector, shared local proportion
validation, sender blocking, and receiver filtering. Detector performance and
classification exceptions still need validation before implementation is final.

Validation to date: local source and architecture inspection plus the earlier
upstream documentation investigation. No application code, dependencies, tests,
or deployed settings have changed. This proposal remains on the dedicated
`feat/english-only-dm-translation` worktree branch; its existing branch name does
not imply translation is still in scope.
