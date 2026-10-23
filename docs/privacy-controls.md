# Privacy controls

Open **Settings → Privacy & security** to choose and apply a device privacy preset. The presets
change notification previews, automatic locking, and sensitive copying:

| Level              | Message previews | Automatic lock   | Lock when hidden | Copy confirmation | Clipboard clear  |
| ------------------ | ---------------- | ---------------- | ---------------- | ----------------- | ---------------- |
| Balanced (default) | On               | Off              | Off              | Off               | Off              |
| Private            | Off              | After 10 minutes | Off              | Off               | After 60 seconds |
| Strict             | Off              | After 1 minute   | On               | On                | After 30 seconds |

Choose **Apply preset** after selecting a preset. Notification preview has its
editor in **Notifications & calls**; Privacy & security shows its current value
and links to the same editor. Changing an individual protection switches the
level to the derived **Custom** state. Network and relay permissions are unaffected.
**Lock now** hides chats while background delivery continues. macOS unlock uses
system device-owner authentication (Touch ID or the Mac login password); other
platforms use the Mesh-Talk login password. Clipboard clearing is best effort and only clears
text while the clipboard still holds this app's copy.

## Protected messages

Choose a protection mode beside the message composer before sending text or an image:

| Mode | Viewing behavior |
| --- | --- |
| Normal | Display the message normally. |
| Click to view | Conceal the body until the viewer explicitly reveals it. |
| Verify to view | Require the same authentication as the app lock before revealing the body. |

The choice applies to text messages, including text replies, and images selected,
pasted, dropped or captured as screenshots. It returns to Normal after successful
acceptance. A failed send retains its protection mode for retry. Videos, generic
file attachments, folders and stickers do not support protected sending; they
cannot be sent in a protected mode. Protected images use the existing supported
raster-image formats and 16 MiB inline-image limit; unsupported formats and larger
images are rejected before acceptance.

The message keeps its place, author, time and delivery status. A reveal displays
only that message for 30 seconds, beginning when its content appears. Reading
or interacting with it does not extend the deadline. Use **Hide** to conceal it
sooner. Leaving the conversation, moving away from the app, locking or signing
out also conceals it. Each subsequent reveal starts a new 30-second period;
Verify to view requires authentication again. Each device reveals independently,
including the sender's devices. Opening a message never unlocks the app or
other messages.

Protected content is omitted from ordinary history responses, notification and
conversation previews, reply quotes, accessibility summaries and text search.
The desktop retrieves the body through a separate exact-message command after
checking the current local user, desktop session and participant runtime. A
message recalled or expired while authentication was pending cannot be revealed.
Revealed content stays in the local message view rather than the shared chat
cache. Protected images have no thumbnail before reveal, and their temporary
preview is released when concealed. Ordinary image reads and exports cannot
bypass protection; copying, forwarding and saving are unavailable for protected
messages.

The 30-second period controls screen visibility, not message retention. Existing
expiry and recall rules continue to apply. Protected content remains in the
encrypted history store and follows ordinary eligible history synchronization
and recovery. Clients that predate protected messages cannot reveal them;
the protected framing prevents their ordinary text decoder from exposing the
body. Use an updated client on each viewing device.

These controls reduce accidental disclosure by supported clients. They cannot
prevent screenshots, photographs, a modified recipient client or inspection of
an already compromised device, and do not promise secure erasure of browser
memory after display.

## Network privacy and contact lists

**Settings → Connection & delivery → Invisible mode** stops public presence
announcements. Accounts you manually allow under **Reply permissions**, and
accounts you contact first, can reach you while invisible. Existing joined
groups keep group-only access. The permission list is saved for the signed-in
user on this device; it does not sync to other devices. Offline relay delivery
requires a manually allowed trusted relay account. Invisible mode is not IP
anonymity: old packets and a network port scan can still reveal the device.

The same **Connection & delivery** page separates:

- **Use post offices**: master switch for relay data and directory participation.
- **Send / Receive through post offices**: independent content directions. Sending
  off also withholds relay receipts and file decisions; receiving off prevents
  content admission but is not a zero-wire-byte guarantee.
- **Discover peers through a post office**: opt into public participant introductions.
- **Device discovery scope → Local network only**: exclude routed public discovery
  (including Tailscale and EasyTier) and bridge directories, while retaining
  permitted physical-interface disclosure. **Local and routed networks** allows
  these optional discovery sources; post-office discovery stays a separate choice.
- **Preferred post office**: select a known direct LAN/Tailscale candidate and
  retain alternatives. A known candidate is rechecked when actually connected.

These are device-local choices. Directory opt-out and physical-only discovery do
not revoke an explicit authorized contact or create an IP firewall. Direct peer
communication remains independent of relay direction settings. The full matrix
and startup/live-change contract are in the
[network baseline](network-design-reference.md#4-settings-and-deployment-configuration).

**Settings → Privacy & security → Hidden contacts** removes selected accounts from your
local contact lists and direct-message search. It keeps their history and does
not stop their messages or calls. Shared groups are unchanged. This is a local
view setting, separate from invisible mode.

Right-click an offline contact and choose **Delete contact** to remove it from
this device's contact list and permanently erase its local message history after
confirmation. This cannot be undone; other devices keep their own history.
Deletion survives restarts and is separate from Hidden contacts. Verified identity
bindings remain intact. Fresh online discovery adds the contact back without
restoring deleted history. Deletion does not block new messages or change shared
groups. The app checks presence again before deleting.

## Limits

Files exported outside the encrypted app store and text copied to the clipboard
can be accessed by the operating system or other apps. A company-managed device
may observe the screen, input, files, and network activity. These controls reduce
app-created exposure; they cannot prevent device-level monitoring.

For implementation details, see [the architecture](ARCHITECTURE.md). For backup
limits, see [backup and recovery](backup-recovery.md).
