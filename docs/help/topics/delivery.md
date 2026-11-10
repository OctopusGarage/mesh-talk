# Understand message delivery

Distinguish a local send, relay custody and confirmation from the recipient account.

## Read the message state

A local message can be pending or failed before recipient confirmation. A relay may hold encrypted events while the recipient is offline. The app's delivery confirmation denotes account receipt, not that a person has read the message.

## Inspect delivery details

Select the clock icon beside the timestamp on an accepted direct message to
expand **Awaiting receipt** details below that message. A check icon denotes
recipient confirmation; select it to inspect its meaning. Status details stay
in the conversation flow without hover overlays. Focus an icon and press Enter
or Space to toggle its details; Escape closes them and returns focus to the icon.

When available, details show when a local attempt started or a retry was
scheduled. The age describes that local observation, not the recipient's
presence or a promised retry time. Older observations can remain after the
connection changes. Missing details mean no current observation is available,
including after restart or eviction; they do not establish a delivery failure.
Automatic delivery continues without creating another copy of your message.

When the client has supporting local evidence, details also explain that
post-office sending was disabled with no known direct route, no eligible route
was known, or an exchange timed out. These observations describe the last local
attempt and do not establish that the recipient is offline. **Review delivery
settings** opens the relevant connection control; **Open diagnostics** opens
the existing connection checks. Neither action resends the message or changes
your settings automatically.

An open recovery dialog stays open when recipient confirmation arrives or its
message scrolls out of view. Dismissing it returns keyboard focus to the delivery
icon if it is still present, or to the conversation shell. Leaving the
conversation, locking the app or changing the signed-in session closes it.

Recipient confirmation replaces the awaiting disclosure. For files, confirmation
applies to the file card; downloading the bytes and reading the file remain
separate. Group copy information and Message Yourself retain their own meanings.
Use **More → Connection** to inspect connection settings and diagnostics.

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

Choose an expiry duration in the composer before sending. The countdown starts when you send and appears beside each expiring message. Updated clients remove the message from both sender and recipient history when the deadline passes, even while the conversation stays open. Delayed arrivals have only the remaining time; messages arriving after their deadline are hidden. Reading does not restart the countdown.

Expiry does not change messages already sent. Messages sent to yourself do not expire automatically. Locally managed media is removed with the expired message; separately saved or copied content can remain. A recipient can still copy content or modify their client, so remote deletion cannot be guaranteed. Local history retention is configured separately in [History & backup](history.md).

## Further reference

[Delivery and network model](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/network-design-reference.md)
