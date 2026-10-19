# Incoming direct-message language

In **Settings → General → Incoming message language**, choose a language to ask
other people to write mainly in that language when messaging your account.
**Unrestricted** is the default. Supported choices are English, Chinese,
Japanese, Spanish, French, German, Korean, Portuguese, Russian, and Arabic.
The interface language remains a separate setting.

Each message needs at least **80%** target-language content. Up to **20%** may
use other languages. There is no percentage slider. The check is per message,
not an average over a conversation. Mixed-language detection is approximate,
particularly for short phrases, related languages, and names.

The proportion counts visible alphabetic/text graphemes in detected language
spans. It excludes punctuation, whitespace, numbers, emoji, and bounded complete
code spans, ASCII URLs and mentions. Adding neutral content cannot raise the
percentage. Common `OK`/`okay` acknowledgements are allowed. These exceptions are
conveniences, not a defense against someone deliberately disguising prose as code.

Detection runs locally using bundled models. No translation, provider account,
API key, internet request, or billing is involved. To bound local processing,
restricted messages must fit within 4,096 UTF-8 bytes, 256 Unicode words, and 64
script runs; longer or more complex messages must be split. Neutral exemptions
also have length bounds. Unsupported or unclassified prose counts as other
language content.

## Sending and receiving

Updated senders see the recipient's known language preference in the conversation.
A blocked message stays in the composer so it can be edited. Final core
acceptance checks the current cached preference again; a successful preview is
not permission to bypass a later preference change. Groups, outgoing messages to
other accounts, attachments, stickers, and calls are unaffected.

The receiving node independently applies its own rule to new incoming text,
including messages queued before the sender learned the preference. Disallowed
content has a content-free placeholder in normal history and live events, is
excluded from search, and is not exported as plaintext to linked-device history.
The underlying signed encrypted event is unchanged. A delivery receipt still
means durable endpoint receipt, not reading or language approval.

Receive decisions are stored durably against content-addressed event IDs before
message acceptance. Restart, changing the rule, and classifier updates do not
silently reveal a previously hidden message or rewrite already accepted history.
The original plaintext remains in the encrypted local message store and follows
its normal retention; this is presentation filtering, not content erasure.

## Devices, offline operation, and compatibility

The preference is account-signed and sealed to peer devices in a versioned full
profile. Profile synchronization also carries it to authorized linked devices
when their **Profile** synchronization category is enabled. Peers cache verified
preferences for offline sends; disconnected devices learn changes when they
synchronize. The receiver's known local rule determines what its normal chat
view exposes.

A newer avatar-only update from an older linked device preserves the modern
local language preference. Ordinary legacy avatar profiles remain byte-compatible
when no language preference has ever been configured. Once an account publishes
language-aware profiles, old clients cannot decode that new profile framing and
need an update for profile/avatar propagation. Text encryption and text-message
framing are unchanged. Old or modified clients can still transmit ciphertext;
receiver filtering handles the displayed content.

Backups include language preferences and receive decisions using snapshot schema 2. This build still reads schema 1 backups; older builds reject schema 2 backups.
The snapshot preserves filtering decisions without altering signed chat events.

## Validation boundary

Focused tests cover the supported languages, grapheme proportions, exact 80/20
boundaries, neutral padding, input bounds, sender admission, policy changes during
acceptance, two-node encrypted loopback delivery/receipts, reused logical IDs,
linked-device backfill, profile persistence, restart, and backup restoration.
Frontend tests cover settings failures and account isolation plus asynchronous
validation/admission and draft preservation. These are bounded automated checks,
not a guarantee of correct classification for every human-language input.
