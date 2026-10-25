# Mesh-Talk domain language

Mesh-Talk supports private conversations between authenticated participants across
local and existing routed networks, with optional peer-operated encrypted custody.
This is the canonical glossary; [DDD conventions](docs/domain-design.md) own its
use in code, ownership decisions and documentation.

## Identity and access

**Local user**:
A local sign-in identity and its protected application data. A local user is not
the cross-device mesh account or a network device.
_Avoid_: account ID, device ID when referring to the local login identity.

**Username / account login name**:
The name entered at sign-in. Linked desktop devices use the kept account's same
login name and password. It is distinct from the account nickname and from the
immutable local storage username retained internally for an existing local user.
_Avoid_: account ID, device ID, nickname, storage directory as interchangeable names.

**Local storage username**:
The existing per-device storage namespace for a local user. Account login
migration preserves this name and the local-user UUID; a credential envelope maps
the shared login to the retained local owner and its separate storage secrets.
_Avoid_: account login name, account ID, shared password.

**Display name**:
A human-readable label for presentation. It is neither unique identity nor
proof of authority.
_Avoid_: identifier, verified identity.

**Account**:
A cryptographic identity shared across certified devices. Account identity does
not depend on a desktop login, IP address or display name.
_Avoid_: local user, machine, device.

**Account profile**:
Account-authenticated nickname, biography and avatar, saved through ordinary
profile editing and synchronized automatically. It carries read-only creation
metadata when known; revisions order updates across devices.
_Avoid_: local login profile, device identity, proof of a person's identity.

Linked devices share account login credentials through private account access. Login names remain distinct from public nicknames; device identities and local encrypted-storage secrets remain distinct.

**Device**:
A participant identified by its persistent signing and key-agreement identity.
Restarting a process does not by itself create a new device.
_Avoid_: running instance, account, physical computer.

**Device certificate**:
An account-signed binding of a device's public identity to that account.
_Avoid_: discovery record, membership grant.

**Account group role**:
The account's accepted member or owner role in a channel. Certified devices
present the same account role. The public membership state and verified account
bindings establish that role; device reachability determines execution
availability, not a different role. Legacy public controls retain their existing
signer requirements.
_Avoid_: online status, device approval, shared group reference.

**Account group operation**:
An account-authorized intent to change channel state or send channel content.
Its pending identity is distinct from the public events that commit its effects.
Execution may use an authorized device without changing the account's role or
reattributing existing public events.
_Avoid_: committed event before acceptance, device-specific permission.

**Peer**:
Another known network device, including an infrastructure device when the role
permits it. A peer is not necessarily a person or a channel member.
_Avoid_: user, contact, member as interchangeable names.

**Contact**:
An account represented in local conversation/contact state. Contact visibility
is separate from discovery, delivery permission and channel membership.
_Avoid_: online peer, accepted member.

**Desktop session**:
One authenticated local user's login lifetime. A subsequent login starts a new
lifetime even when it belongs to the same local user.
_Avoid_: transport session, ratchet session, account lifetime.

**Participant runtime**:
The active network-participation lifetime of an initialized node. It is distinct
from persistent device identity and the host's login lifetime.
_Avoid_: device, desktop session.

**Integration grant**:
An authorization for a local client to perform specified integration operations
for permitted destinations within its validity period.
_Avoid_: channel membership, device certificate, session token.

## Conversations and history

**Conversation**:
A direct or channel conversation whose authorized content can be retained and
synchronized. A UI conversation selection is a view of that conversation.
_Avoid_: transport connection, multicast group.

**Direct message (DM)**:
A message in a direct conversation between account or legacy device identities.
It is not necessarily transmitted over a direct network route.
_Avoid_: direct connection, LAN-only message.

**Channel**:
A conversation governed by signed membership and control state. “Group” is an
accepted user-facing label for this concept.
_Avoid_: multicast group, Noise channel when referring to conversation membership.

**Channel member**:
A signed account grant admitted by the channel's membership state. Certified
devices inherit that account's accepted membership and role without another owner
approval. Legacy channels retain device-scoped authority until a validated,
owner-signed account migration; discovery alone never creates membership.
_Avoid_: discovered peer, invitee, online person.

**Event**:
An immutable, signed, content-addressed protocol record with authenticated
ancestry. Events can represent messages, reactions, membership changes or other
protocol facts; not every event is a user-visible message.
_Avoid_: UI notification, IPC callback, message as a universal synonym.

**Message**:
Conversation content presented as one logical message. Pending UI content and
protocol records are different representations, not interchangeable identities.
_Avoid_: Event ID before acceptance, transport frame.

**Outgoing intent**:
A local attempt to send content to a conversation before or while acceptance is
resolved. Its local identity is distinct from an accepted event's identity.
_Avoid_: delivered message, accepted event before acceptance.

**Protected message**:
A text or image message carrying an authenticated requirement to conceal its content
until an explicit local reveal, optionally after the viewer authenticates with
the application's lock mechanism. Each reveal lasts at most 30 seconds on that
device. Protection is separate from retention, message expiry and read state.
_Avoid_: disappearing message, view-once message, password shared by the sender.

**History coverage**:
What this installation has observed of a conversation's retained events. It is
not proof of global history completeness or a read position.
_Avoid_: synchronization complete, all messages received.

**Message Yourself**:
The current account's personal conversation. Its existence does not require a
second device to be discoverable.
UI label: **Message Yourself** (Simplified Chinese: **给自己发消息**).
_Avoid_: second account, temporary local scratch chat.

**Message favorite**:
An account-owned saved item containing one or more ordered message snapshots and
optional personal title, description and per-message notes. It is independent of
ordinary chat retention; explicit message expiry still applies to saved content.
Collection UI label: **Favorites** (Simplified Chinese: **收藏夹**). An item without
a title is an **Untitled favorite** (**未命名收藏**). Do not use “Saved Messages”
as either feature's UI label.
_Avoid_: Message Yourself, self-chat, pinned contact or pinned conversation.
See [Favorites](docs/favorites.md).

**Recovery archive**:
A user-created encrypted snapshot protected by a separate recovery key. Desktop
backups reuse a key acknowledged once and retained in encrypted local-user storage;
older archives still need their original keys. Restore retains account identity
but creates a new device identity. See [backup and recovery](docs/backup-recovery.md).
_Avoid_: device clone, device revocation, live synchronization.

## Networking and availability

**Sticker media**:
A single encrypted image attachment with a source identity and text fallback.
Its exact content is bound by the attachment checksum. Receiving and retaining
the image is independent from installing its source pack in the sticker picker.
Legacy ID-only stickers remain dependent on installed artwork. See the
[delivery contract](docs/CUSTOMIZATION_PACKS.md#sticker-message-delivery).
_Avoid_: installed pack, delivered image and locally available bytes as synonyms.

**Host machine**:
A physical or virtual computer that can run multiple participant processes with
different device identities.
_Avoid_: device, peer as a hardware synonym.

**Discovery**:
Learning identity and route candidates from supported observations. Discovery
alone establishes neither conversation permission nor successful transport.
_Avoid_: authorization, delivery, presence as an interchangeable operation.

**Presence**:
This client's eligible public evidence of a participant's availability.
Fresh evidence and displayed online tolerance are distinct: pure local discovery
expires at 15 seconds; cross-network public observations retain Online for up to
120 seconds from the original observation, with a concise slow-connection hint
when freshness expires. This tolerance counts consistently in UI online totals,
but does not create fresh device evidence or prove delivery. Authoritative
withdrawal ends it immediately. Offline is client-relative, not global sign-out.
_Avoid_: global online state, guaranteed connectivity.

**Roster**:
A projection of known devices and their observations. A raw roster can include
infrastructure and does not define channel membership.
_Avoid_: member list, people list without role/account projection.

**Physical LAN**:
An interface/subnet discovery scope subject to isolation and firewall policy.
A routed VPN link does not merge neighboring LAN discovery scopes.
_Avoid_: every directly reachable device.

**Direct application route**:
A candidate path to a device without an intermediate Mesh-Talk post office.
The underlying network can itself use a VPN relay.
_Avoid_: physical LAN, guaranteed connection, direct-message conversation.

**Route provenance**:
The source of a learned route, such as local discovery, a routed query or a
post-office introduction. An address prefix is not provenance.
_Avoid_: physical location inferred from an IP address.

**Registration origin**:
An explicitly shared, short-lived relationship between a participant device and
the post office with which it registered. Both parties authenticate the binding;
forwarding preserves its original expiry. It is distinct from the immediate
directory introducer and does not establish physical location. Sharing defaults
off and is controlled by the registering participant device.
_Avoid_: geographic origin, home network, delivery route, inferred location.

**Post office**:
A peer-operated participant that temporarily retains opaque encrypted events.
“Relay” is an accepted role label here; custody does not confer channel membership.
_Avoid_: central server, group member, generic IP router.

**Bridge mode**:
A post office's opt-in directory capability. Client permission to use bridge
discovery and relay federation enrollment are separate choices.
_Avoid_: merged subnets, automatic federation trust.

**Directory introduction**:
Eligible participant information learned through a bridge. Its mailbox route
must not be mistaken for the participant's own transport listener.
_Avoid_: direct endpoint, participant's physical machine address.

**Presence source office**:
The identified visible directory introducer closest to a device's original
presence registration by known trusted forwarding distance. It is client-relative;
an unseen origin can be represented by the nearest visible introducer. Unknown
legacy distance remains unknown. Multiple devices may have different sources.
_Avoid_: physical location, first responder, selected delivery relay.

**Federation**:
Authenticated exchange among operator-approved, equally eligible post offices.
Approval uses explicit device pins or authority-signed network memberships;
network reachability and co-location do not constitute enrollment.
_Avoid_: trust of every discovered relay, mandatory global hub.

**Relay network membership**:
An expiring, nondelegating operator-signed approval of one device fingerprint
for relay reconciliation. Each relay verifies it against a locally approved
network authority and revocation snapshot, bound to the authenticated Noise peer.
It does not grant conversation membership or attest software integrity.
_Avoid_: official-client verification, malware-free device, account membership.

**Anti-entropy**:
Bounded reconciliation of missing authenticated events between authorized holders.
It depends on retained data and available paths.
_Avoid_: guaranteed complete recovery.

**Privacy generation**:
A local policy lifetime used to invalidate work admitted under superseded privacy
permissions. It is separate from account identity and desktop-session lifetime.
_Avoid_: account version, transport session, permanent revocation proof.

## Acceptance and delivery

**Acceptance**:
The core's successful admission of outgoing content into its delivery workflow.
It is distinct from recipient delivery and can outlive a caller timeout.
_Avoid_: sent to everyone, delivered, read.

**Custody**:
Retention of an encrypted copy by an eligible post office. It is not recipient
confirmation, permanent durability or proof that attachment bytes are available.
_Avoid_: delivered, read, backed up forever.

**Recipient confirmation**:
Validated evidence bound to the intended destination. A transport acknowledgment
or relay acceptance cannot substitute for this evidence.
_Avoid_: TCP success, custody receipt as proof of recipient delivery.

**Read state**:
Local observations or explicit actions indicating viewed conversation content.
It is separate from receipt, history coverage and public read receipts.
_Avoid_: delivered, globally read.

**File availability**:
Whether authorized attachment bytes can be obtained and verified. A retained
message or file manifest alone does not establish byte availability.
_Avoid_: file downloaded because its message arrived.

**Group history replica**:
A retained authorized copy served by an accepted channel member. It is distinct
from an opaque post-office copy whose holder cannot decrypt channel content.
Displayed copy confirmations count distinct external member accounts, excluding
the sender's linked devices. The sender account can share its historical
observations privately across devices; unknown status is separate from zero.
_Avoid_: post-office membership, global archive.

**Join-history copy**:
A bounded, owner-authorized copy of retained channel text offered for a particular
invitation and readable only after acceptance. Original IDs and authored times
are retained; the owner attests to the copied content. It does not grant old
sender-chain keys, attachment access or complete conversation history.
_Avoid_: full history sync, original-author signature on the copied plaintext.
See [channel history settings](docs/help/topics/channels.md#history-for-new-members).

## Peer trust evidence

- **Public profile signature**: authenticates the Account publishing a profile,
  not a person's identity or honesty. It never grants private verification.
- **Account creation time**: immutable metadata recorded with a newly generated
  Account key using the originating device's clock. Unknown legacy dates remain
  unknown; local User registration, backup dates and retired self-declared card
  claims are different facts.
- **First observed by this installation**: private local evidence of the first
  recorded certified Account/Device observation. It is never a public account age.
- **Legacy trust card**: a frozen signed profile/proof format retained only for
  old events and backups. Introductions and creation-witness receipts provide no
  current application workflow or verification authority.

The [profile and verification guide](docs/peer-trust.md) owns evidence limits and sharing rules.

**Private account document**:
A typed, account-authenticated versioned record synchronized only among certified
sibling devices under its domain policy. Optional personal data follows its
consent category; group-directory metadata follows the existing mandatory account
topology contract and contains no message bodies or keys. Its signed metadata and deletion
tombstone are distinct from the encrypted content-addressed blob and from public
group authority events.
_Avoid_: arbitrary replicated settings file, public group patch, delivery receipt.

**Shared group reference**:
An account-private descriptive snapshot supplied by a certified device that
legitimately holds the group. It preserves the group's exact identity and known
member facts before another device obtains validated group state. It does not
create membership, management authority, sending permission or receiving keys.
_Avoid_: accepted invitation, backup restoration, device approval, authority grant.
