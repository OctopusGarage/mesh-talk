# Create and manage a channel

Start a group conversation and review its members from the conversation header.

## Start a channel

1. Use the sidebar's **New channel** control.
2. Enter a name and select the available people you want to invite.
3. Choose **Create** and inspect any error before trying again.
4. Open the new channel from the sidebar to send messages.

## Review membership

Open the channel's members control in the conversation header. Member profiles show known account and device identities. A known endpoint can belong to a relay; it is not proof of the person's physical location.

Membership and cryptographic access are not interchangeable with merely knowing a channel name. After restoring a backup onto a new device, old channel messages may be readable as an archive while future channel messages require a new invitation from the owner.

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

An invitee receives earlier content only after accepting. The owner shares a
separate encrypted copy; old sender-chain keys are never distributed. The copy
keeps original message IDs, attribution and timestamps, and arrives silently as
history. The owner attests to copied content; it is not a new message signed by
the original sending device. Attachment/file contents are not included in this
initial history mechanism, and reply previews do not unlock messages outside the
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

## Read messages after reconnecting

An active accepted group can show that it is waiting for a reachable member, exchanging retained history, or has completed a peer check. A completed check describes this device's observation; offline members can still hold additional messages.

Recovered messages keep their original authored time and follow verified conversation history. They can appear earlier in the conversation and remain unread until viewed or explicitly marked read. Use **New messages** or **Earlier messages received** to navigate without losing your place. See [History](history.md) for reading and retention.

## Further reference

[Group reception and recovery limits](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/group-conversation-reception.md)

[Channel and identity architecture](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/ARCHITECTURE.md)
