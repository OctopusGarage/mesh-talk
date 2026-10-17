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
**Lock now** hides chats while background delivery continues. Unlock with the
account password, or with Touch ID on supported macOS systems when **Stay signed
in** has a saved credential. Clipboard clearing is best effort and only clears
text while the clipboard still holds this app's copy.

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
- **Physical LAN discovery only**: exclude routed/Tailscale public discovery and
  bridge directories, while retaining permitted physical-interface disclosure.
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

## Limits

Files exported outside the encrypted app store and text copied to the clipboard
can be accessed by the operating system or other apps. A company-managed device
may observe the screen, input, files, and network activity. These controls reduce
app-created exposure; they cannot prevent device-level monitoring.

For implementation details, see [the architecture](ARCHITECTURE.md). For backup
limits, see [backup and recovery](backup-recovery.md).
