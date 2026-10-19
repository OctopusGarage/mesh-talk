# Understand message delivery

Distinguish a local send, relay custody and confirmation from the recipient account.

## Read the message state

A local message can be pending or failed before recipient confirmation. A relay may hold encrypted events while the recipient is offline. The app's delivery confirmation denotes account receipt, not that a person has read the message.

## If a message stays pending

1. Check the connection status and open **More → Connection**.
2. Confirm the intended recipient account and its reachable devices.
3. For direct delivery, check network reachability and the recipient's admission or invisible-mode permissions.
4. For offline delivery, check whether an eligible post office and the required send/receive directions are available.
5. Inspect any error shown on the message before retrying.

Seeing a peer in discovery does not prove that the transport or recipient admission succeeded. A preferred post office is not proof of custody. Read [Find people](find-people.md) and [Post offices](post-offices.md) for those boundaries.

## Recover group messages after reconnecting

A reachable, authorized group member with sharing enabled can supply retained
messages even when the original sender is unavailable. This is separate from
post-office custody. A completed local peer check does not prove that every
offline member has no additional history. See [Channels](channels.md) for sharing
controls and recovery status, and [Files](send-files.md) for attachment availability.

## Expiring messages

A message expiry is a recipient-side retention feature enforced by updated clients. A recipient can still copy content or modify their client; remote deletion cannot be guaranteed. The sender retains their local copy. Local history retention is configured separately in [History & backup](history.md).

## Further reference

[Delivery and network model](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/network-design-reference.md)
