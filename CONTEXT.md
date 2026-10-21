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

**Username**:
The local user's login name. It is distinct from an editable display name.
_Avoid_: account ID, peer identity.

**Display name**:
A human-readable label for presentation. It is neither unique identity nor
proof of authority.
_Avoid_: identifier, verified identity.

**Account**:
A cryptographic identity shared across certified devices. Account identity does
not depend on a desktop login, IP address or display name.
_Avoid_: local user, machine, device.

**Account profile**:
Signed account-level avatar state, with
revisions that order updates across devices.
_Avoid_: local login profile, device identity, unverified display label.

**Device**:
A participant identified by its persistent signing and key-agreement identity.
Restarting a process does not by itself create a new device.
_Avoid_: running instance, account, physical computer.

**Device certificate**:
An account-signed binding of a device's public identity to that account.
_Avoid_: discovery record, membership grant.

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
A device admitted by the channel's membership state. A person-facing account
count can combine certified devices without changing device membership authority.
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

**History coverage**:
What this installation has observed of a conversation's retained events. It is
not proof of global history completeness or a read position.
_Avoid_: synchronization complete, all messages received.

**Saved Messages**:
The current account's personal conversation. Its existence does not require a
second device to be discoverable.
_Avoid_: second account, temporary local scratch chat.

**Recovery archive**:
A user-created encrypted snapshot protected by a separate recovery key. Restore
retains account identity but creates a new device identity.
_Avoid_: device clone, device revocation, live synchronization.

## Networking and availability

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

**Federation**:
Authenticated exchange among explicitly enrolled, equally eligible post offices.
Network reachability and co-location do not constitute enrollment.
_Avoid_: automatic trust, mandatory global hub.

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
_Avoid_: post-office membership, global archive.
