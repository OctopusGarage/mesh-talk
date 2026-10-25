# Create and manage a channel

Start a group conversation and review its members from the conversation header.

## Start a channel

1. Use the sidebar's **New channel** control.
2. Enter a name and select the available people you want to invite.
3. Choose **Create** and inspect any error before trying again.
4. Open the new channel from the sidebar to send messages.

## Review membership

Open the channel's members control in the conversation header. Member profiles show known account and device identities. A known endpoint can belong to a relay; it is not proof of the person's physical location.

Membership belongs to your account. Linking another device does not require the
group owner to invite or approve that device again. An accepted, reachable device
on your account can provide the verified group state and fresh receiving keys;
the owner can be offline. Your devices keep independent sending keys.

Older device-only groups require a compatible client update and an automatic
owner-signed migration before this inheritance is available. No manual approval
is required for each device. Knowing a group name or restoring an archive alone
does not grant access, and historical messages still require an authorized
retained copy.

## Groups across your devices

Your linked devices share the group directory and known member details even
when a legacy group has not completed its format upgrade. Group names are labels:
two groups with similar names remain separate when their IDs differ.

A shared reference makes the group visible while its verified group state is
being obtained. It does not grant membership or copy sending keys. The app keeps
that reference read-only and explains the group-format requirement; it must not
describe it as a restored backup or request separate approval for every linked
device. The original device keeps its existing group access.

Group-directory metadata is account topology. Turning off optional message
history does not hide the groups already held by your account's devices. It also
does not enable message-history transfer or change any other synchronization choice.

**Previous account groups** contains local history whose account-authorized
membership still belongs to the account used before linking. You can expand this
section to read retained messages. These rows are separate from your current
account's group directory and do not offer joining or sending actions. Linking
devices does not merge two separate accounts' group memberships or ownership.

## Understand message copy status

A group message's check marks report confirmed stored copies held by other
member accounts. They do not mean everyone received or read the message. Copies
on your own linked devices do not count as another member's confirmation.

Your linked devices synchronize these historical confirmations with message
history enabled. **Copy status unavailable** means this device has not obtained
the status yet; **No member copy confirmed** means no eligible member copy is
currently confirmed in the available evidence. A post-office copy has a separate
label. A holder can later delete a copy, so an old confirmation is not a permanent
availability guarantee.

## History for new members

The owner can open the channel's member controls and change **History for new members**.
Other members can view this setting. Pending invitees and closed channels cannot change it.

| Setting | Earlier text messages included |
| --- | --- |
| Off | None |
| Last 10 messages | Up to 10 retained text messages |
| Last 3 days | Text messages authored in the preceding 3 days, within transfer limits |
| Last 10 messages within 3 days | Up to 10 text messages from the preceding 3 days |

New invitation-based groups default to **Last 10 messages within 3 days**. Existing
channels without a signed policy remain **Off**. The policy and selection are frozen
when the owner invites a device, using that invitation's timestamp. Changes apply
to future invitations; they cannot retract copies already received.

In account-authorized groups, an invitation belongs to the account. Another
certified device can accept it while the originally invited device is offline;
an eligible reachable holder can release the same selected history to that device.
The owner does not approve each device separately.

An invitee receives earlier content only after accepting. The owner shares a
separate encrypted copy; old sender-chain keys are never distributed. The copy
keeps original message IDs, attribution and timestamps, and arrives silently as
history. The owner attests to copied content; it is not a new message signed by
the original sending device. Attachment/file contents are not included in this
initial history mechanism. Protected text and images are excluded before applying
message-count and transfer limits. Reply previews do not unlock messages outside the
selected copy.

Copies are bounded to 100 text messages and 24 KiB of encrypted row data; the
10-message options apply the smaller limit. The owner considers its latest 500
retained conversation entries, omitting file cards, recalled/expired content and
future timestamps. Fewer messages can be available. Deadlines and recalls are
checked again before individual message keys are released.

A reachable authorized sharing holder must retain the invitation's copy. Up to 64
existing devices already authorized for every selected message can help release
it; a later member does not gain older history merely by being a possible replica.
If no eligible holder is reachable, delivery waits. Removing and re-inviting a
device starts a new history permission lifetime. Upgrade all participating clients
before enabling this feature: history controls require version 3 channel sync,
and older sync versions receive an upgrade response rather than unknown events.

## Share retained history

Open the members control and find your own accepted membership. **Stop sharing** disables this device's **Share a copy** participation; **Resume sharing** enables it again. Sharing lets authorized members obtain retained channel history and available attachments from this device. Turning it off does not stop incoming messages or recovery of your own authorized history. Upgrade all members before using these controls with older clients.

A reachable member must retain the needed events and keys and permit sharing for member recovery to work. An opaque post office can hold encrypted copies but cannot grant membership or recover missing endpoint identity keys. A retained message or file card does not guarantee that attachment contents remain available.

### Network data protection

The desktop temporarily pauses sharing other members' attachment contents when
the system reports a metered/expensive network, a data restriction, or unknown
network cost. Messages and your original file sends continue. **Share a copy**
stays enabled and cached files remain subject to their normal retention rules.
Sharing resumes automatically when the network is reported unrestricted.

The members dialog shows the pause on this device. **Allow temporarily** permits
file assistance across all groups on this device until the reported cost status
changes or you sign out. **Resume data protection** revokes the exception sooner.
An exception never overrides **Stop sharing** or group access permissions.

Protection covers automatic attachment copies of every size, including images.
It does not set a daily upload allowance. Allowing assistance temporarily does
not reserve or enforce an amount of data that may be uploaded.

A phone hotspot also uses Wi-Fi; system cost detection cannot identify every
hotspot. Local-only transfers on that hotspot may also be paused. Use the
temporary exception when appropriate. VPN and multiple-interface routes can make
system cost information incomplete. Already buffered traffic can finish after a
network switch; this feature does not cap all application traffic or change
downloads, original file sends, backups or post-office service behavior.

## Read messages after reconnecting

An active accepted group can show that it is waiting for a reachable member, exchanging retained history, or has completed a peer check. A completed check describes this device's observation; offline members can still hold additional messages.

Recovered messages keep their original authored time and follow verified conversation history. They can appear earlier in the conversation and remain unread until viewed or explicitly marked read. Use **New messages** or **Earlier messages received** to navigate without losing your place. See [History](history.md) for reading and retention.

## Further reference

[Group reception and recovery limits](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/group-conversation-reception.md)

[Channel and identity architecture](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/ARCHITECTURE.md)
