# Privacy controls

Open **Settings → Privacy & security** to choose a privacy level. The presets
change notification previews, automatic locking, and sensitive copying:

| Level | Message previews | Automatic lock | Lock when hidden | Copy confirmation | Clipboard clear |
|-------|------------------|----------------|------------------|-------------------|-----------------|
| Balanced (default) | On | Off | Off | Off | Off |
| Private | Off | After 10 minutes | Off | Off | After 60 seconds |
| Strict | Off | After 1 minute | On | On | After 30 seconds |

Changing one of these controls individually switches the level to **Custom**.
**Lock now** hides chats while background delivery continues. Unlock with the
account password, or with Touch ID on supported macOS systems when **Stay signed
in** has a saved credential. Clipboard clearing is best effort and only clears
text while the clipboard still holds this app's copy.

## Network privacy and contact lists

**Settings → Network privacy → Invisible mode** stops public presence
announcements. Accounts you manually allow under **Reply permissions**, and
accounts you contact first, can reach you while invisible. Existing joined
groups keep group-only access. The permission list is saved for the signed-in
user on this device; it does not sync to other devices. Offline relay delivery
requires a manually allowed trusted relay account. Invisible mode is not IP
anonymity: old packets and a network port scan can still reveal the device.

**Settings → Contacts → Hidden contacts** removes selected accounts from your
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
