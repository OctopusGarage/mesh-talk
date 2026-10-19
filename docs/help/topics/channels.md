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

## Share retained history

Open the members control and find your own accepted membership. **Stop sharing** disables this device's **Share a copy** participation; **Resume sharing** enables it again. Sharing lets authorized members obtain retained channel history and available attachments from this device. Turning it off does not stop incoming messages or recovery of your own authorized history. Upgrade all members before using these controls with older clients.

A reachable member must retain the needed events and keys and permit sharing for member recovery to work. An opaque post office can hold encrypted copies but cannot grant membership or recover missing endpoint identity keys. A retained message or file card does not guarantee that attachment contents remain available.

## Read messages after reconnecting

An active accepted group can show that it is waiting for a reachable member, exchanging retained history, or has completed a peer check. A completed check describes this device's observation; offline members can still hold additional messages.

Recovered messages keep their original authored time and follow verified conversation history. They can appear earlier in the conversation and remain unread until viewed or explicitly marked read. Use **New messages** or **Earlier messages received** to navigate without losing your place. See [History](history.md) for reading and retention.

## Further reference

[Group reception and recovery limits](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/group-conversation-reception.md)

[Channel and identity architecture](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/ARCHITECTURE.md)
