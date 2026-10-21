# Peer trust cards

A trust card is an account-signed public statement. Its signature authenticates
which account published its contents; it does not establish a person's identity,
honesty, or a trustworthy wall clock. Account IDs remain derived from the account
public key and never depend on a profile name or timestamp.

## Evidence and its limits

| Evidence | Meaning | Authority |
| --- | --- | --- |
| Signed name, bio and avatar | The account signed these presentation claims. | Account key |
| Creation date | The account declares this date; it may be backdated. | Account key and owner's clock |
| First observed by this installation | This installation first recorded a certified account/device observation at this local time. | Private local history and local clock |
| Device-change history | This installation observed different certified device identities for the account. A multi-device account can legitimately alternate devices. | Private local history |
| Verified contact | You explicitly compared the device safety number through a trusted independent channel. | Your local decision |
| Signed introduction | An identifiable issuer made the displayed statement about the account. | Issuer key; credibility depends on your relationship with the issuer |
| Creation witness receipt | An independent account states it observed this account's signed creation declaration by its stated local time. | Witness key and clock; not a trusted timestamp authority |

A valid witness signature alone does not establish an accurate date. Multiple
receipts do not create a consensus clock or prevent collusion, and an old account
can be compromised. No global trust score, automatic verification, or human
identity badge is derived from these fields.

New desktop mesh accounts receive a signed creation claim when their account key
is first generated. That timestamp still uses the local clock and remains
self-declared. Missing creation evidence remains unknown. Existing local sign-in user creation
and backup dates are different facts and must not be relabeled as mesh account
creation. Once published, a known creation claim is preserved by the normal edit
path. This application rule does not prevent an account owner using a modified
client to sign a false or conflicting claim.

## Public card and private overlay

Open your profile to publish a signed display name and short bio, with an optional
self-declared creation date. Open a contact's verification dialog to inspect their
public card alongside your private first-observed time and device history.
Account QR codes identify the account; they do not encode verification approval
or the entire signed profile.

Copy or download the signed JSON to share a machine-verifiable artifact. Import
checks the version, bounds, signatures and account bindings. A card import never
marks a contact verified. Introductions and witness receipts can be generated for
a contact, shared with that person and imported into their own card. Witnessing
requires a known creation claim. An issuer's credibility must be checked
separately; a card or screenshot cannot transfer your private verification.

Public exports contain the signed card or proof only. They never contain your
first-observed time, local device-change history, aliases or verification flags.
Sharing a signed card makes its account identifier and included introductions
linkable; publish only information you intend other peers to retain.

Private observation history uses a versioned local format. Legacy trust records
remain readable without inventing observation dates. Older clients see the new
format as unknown trust, so they cannot mistake an original observation for the
current verification pin. Using an older client can replace that local history.

## Implementation ownership

The protocol core owns signed artifact domains, bounded decoding, authenticated
account bindings, durable revisions, encrypted storage and profile-event delivery.
Desktop commands retain exact-session authority through local acceptance and
adapt bounded versioned JSON; they do not implement signatures. The desktop trust
store owns private observation history and explicit device verification. Frontend
adapters pass typed intents, and existing profile/verification dialogs render the
source and limits of each claim.

Legacy avatar profiles remain readable. Each new publication emits the signed
card followed by its legacy avatar under one publication lock. The avatar event
names the card event as a required parent, so missing-card delivery cannot cause
an avatar compatibility update to supersede unseen card metadata. Profile-log
compaction retains the latest unreferenced leaves per author and every referenced
card parent; superseded parents become eligible after their old children are
removed.
Trust cards are an additive capability;
older clients cannot display their new fields. Portable backup records preserve
signed public cards without converting private observations into public evidence.
See [account and device identity](ARCHITECTURE.md), [module ownership](module-boundaries.md)
and [contact verification](help/topics/verify-contact.md).

## Resource bounds

The core accepts at most 256 UTF-8 bytes for the signed name, 2 KiB each for
bio and introduction text, 128 KiB for an avatar, and 16 introductions plus 16
witness receipts per card. Wire decoding rejects inputs larger than 224 KiB
before deserializing. Public JSON import is capped at 1 MiB to allow the byte-array
expansion of an avatar. The encrypted card store retains at most 1,024 current
accounts and 32 MiB of current encoded cards; superseded journal entries are
compacted separately. These are resource limits, not reputation thresholds.
