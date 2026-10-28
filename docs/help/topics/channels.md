# Create and manage a channel

Start a group conversation and review its members from the conversation header.

## Group photo and personal overrides

Open the member control in the conversation header to see **Group photo**.
The current owner account can publish or remove the photo shared with members.
An uploaded image is cropped to 256 pixels and must fit within 32 KiB after
cropping. Choose a simpler image if the size limit is exceeded.

Click the photo in the conversation header for **My group photo**. This choice
is private to your account and follows your appearance synchronization policy.
Your personal photo takes priority over the owner-published group photo. Choose
**Use group photo** to remove your override and follow future group photo changes.
If neither photo exists, the app shows the existing member montage.
Removing the shared photo does not remove anyone's personal override. Ownership
transfer preserves the shared photo for the next owner to manage. Existing private
photos are never automatically published to the group.

## Group announcements

Open the member control and find **Group announcement**. Members can read the
current text; the current owner account can choose **Write announcement** or
**Edit announcement**, then **Preview** and **Publish**. Canceling leaves the
published announcement unchanged. Announcements are plain text, preserve line
breaks and allow up to 4 KiB of UTF-8 text. This byte limit accommodates different
languages without treating character count as wire size.

Publishing adds a card to the conversation from the same accepted group event.
A new announcement from another account also appears as a compact reminder and
a sidebar label. Open it to see the full text, or dismiss that version's reminder.
This device-private action neither deletes the announcement nor sends a read
receipt. Notification preferences remain in effect. A later version can remind
you again. Publishing unchanged text with its current revision does not create
another card or reminder. Earlier cards open the current announcement; removed announcements
show a removal state. Recent publication cards are a bounded presentation window,
not an unlimited announcement archive.

The owner can choose **Delete announcement** and confirm the removal. Deleted
versions retain a tombstone, preventing an offline device from restoring stale
text. A stale editor must refresh and review current information before retrying;
announcement and photo revisions are independent. Ownership transfer preserves
the current announcement and ends the former owner's editing authority.

Current shared photos and announcements are encrypted group content and remain
available independently of ordinary chat retention. After accepting an invitation,
a new member can receive the current group information from an eligible existing
member even when the owner is offline and pre-join message history is off. This
does not release old chat keys or grant access to earlier announcements. Until the current content key arrives, the app reports that it is waiting for
group information rather than reporting that no announcement exists. As with
other peer-to-peer content, an authorized reachable copy is needed for catch-up.
Pending invitations and descriptive shared references do not grant access to the
content or permission to publish it.

## Start a channel

1. Use the sidebar's **New channel** control.
2. Enter a name and select the available people you want to invite.
3. Choose **Create** and inspect any error before trying again.
4. Open the new channel from the sidebar to send messages.

Each account appears once in the invitation picker, regardless of its number of
devices. Your own linked devices are not separate people to invite. A new group
assigns ownership to your account immediately. Invited accounts accept once;
their certified linked devices inherit that membership and role through the
existing authenticated group-state and key exchange. Both people need a reachable,
updated client when creating an account invitation. If its account binding or
format support cannot be verified, creation fails before a group is written.

## Review membership

Open the channel's members control in the conversation header. Each account appears once, with its nickname, avatar, presence and role. Select a person to view their profile or start a private conversation. Device totals, device identities and other members' sharing preferences are not shown in this list. Open **Group settings** for your preferences and the group history policy.

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

An automatic group-format update preserves completed departures. Rejoining
requires a fresh invitation and acceptance.

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

A group message's copy icon reports confirmed stored copies held by other
member accounts. The number beside it is 1 or 2+; a question mark means the
status is unavailable, and the icon alone means no member copy is confirmed.
An inbox icon identifies a post-office copy. Select a status icon to expand its
explanation below the message; select it again or press Escape to collapse it.
These signals do not mean everyone received or read the message. Copies
on your own linked devices do not count as another member's confirmation.

Your linked devices synchronize these historical confirmations with message
history enabled. **Copy status unavailable** means this device has not obtained
the status yet; **No member copy confirmed** means no eligible member copy is
currently confirmed in the available evidence. A post-office copy has a separate
label. A holder can later delete a copy, so an old confirmation is not a permanent
availability guarantee.

## History for new members

The owner can open **Members → Group settings** and change **History for new members**.
Other members can view this setting. Pending invitees and closed channels cannot change it.

| Setting                        | Earlier text messages included                                         |
| ------------------------------ | ---------------------------------------------------------------------- |
| Off                            | None                                                                   |
| Last 10 messages               | Up to 10 retained text messages                                        |
| Last 3 days                    | Text messages authored in the preceding 3 days, within transfer limits |
| Last 10 messages within 3 days | Up to 10 text messages from the preceding 3 days                       |

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

## File delivery

Ordinary files can travel through configured post offices even when members are
on different networks without a direct connection. The sender deposits encrypted
file contents; the recipient can approve and download later while the sender is
offline, provided a reachable post office still holds the needed contents.

Keep the sender available until postal custody has succeeded. A file card or an
online indicator does not prove that the contents reached a post office or the
recipient. Before the first chunk arrives, progress says **Waiting for file
data**; the percentage remains zero. Explicit approval is still required when
automatic receiving is disabled. Refusal prevents local reception and stops
further deposits once the sender learns that decision. Both clients need the
updated file behavior; post-office storage and retention limits still apply.

## Share retained history

Open **Members → Group settings → Your preferences**. **Stop sharing** disables this device's **Share a copy** participation; **Resume sharing** enables it again. Sharing lets authorized members obtain retained channel history and available attachments from this device. Turning it off does not stop incoming messages or recovery of your own authorized history. Upgrade all members before using these controls with older clients.

A reachable member must retain the needed events and keys and permit sharing for member recovery to work. An opaque post office can hold encrypted copies but cannot grant membership or recover missing endpoint identity keys. A retained message or file card does not guarantee that attachment contents remain available.

### Network data protection

The desktop temporarily pauses sharing other members' attachment contents when
the system reports a metered/expensive network, a data restriction, or unknown
network cost. Messages and your original file sends continue. **Share a copy**
stays enabled and cached files remain subject to their normal retention rules.
Sharing resumes automatically when the network is reported unrestricted.

Your group preferences show the pause on this device. **Allow temporarily** permits
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
