# Use your identity on another device

Link devices through Your devices so they share one account, login name and
password. The kept account also supplies the nickname, avatar
and bio. Update Mesh-Talk on both computers before linking.

## Open device linking

Choose which account to keep before entering a code. The device displaying the
code keeps its account. The device entering that code switches to that account.

1. On the computer whose account you want to keep, open **More → Link a device →
   Keep this device’s account**, then choose **Show code for the other device**.
   Keep this dialog open.
2. On a new computer, choose **Link an existing account** from the sign-in
   screen. You do not need to create a temporary account. If already signed in,
   open **More → Link a device → Switch this device to another account**.
3. Select the computer showing the code. Review the source device and the
   current-to-target account IDs, then enter its code within two minutes.
4. Confirm that **this joining device** will switch to the selected device’s
   account, then choose **Use …’s account on this device**. The source computer
   keeps its account, login name and password. The joining computer now uses that
   same login name and password; its previous login no longer works. The kept
   computer shows its login name and nickname separately before issuing its code.

If the source login name already belongs to another local user on the joining
computer, linking fails without replacing that user. Resolve the name conflict
before trying again; Mesh-Talk does not merge local users or invent a new login.
Login credentials are shared regardless of the optional synchronization choices.

**Sync messages, profile and preferences** is selected for this linking flow. Before issuing a code,
the source durably prepares its saved contact and appearance preferences; the
joining computer does not publish blank local defaults over the source’s data.
Both sides enable the five supported sync categories when using this option.
That explicit source choice is signed with the account login record, so an older
linked installation can apply it once after updating. Later manual category
changes remain respected. Turn the option off to retain explicit category choices.
Device-specific permissions, paths and settings remain local; this option does
not promise synchronization of every installation setting or draft.

Treat a linking code as sensitive. Do not share it with someone who should not join your account.

## Connection tips

Linking needs a direct connection between the devices. This can use a shared LAN
or a reachable VPN/routed network; the devices do not have to use the same Wi-Fi.
Messages may work through a post office even when device linking cannot.

- Keep both devices awake and running Mesh-Talk, with the source signed in. Leave the current
  code displayed on the original device. A code expires after two minutes and
  can be used once; spaces in a copied code are accepted by the device dialog.
- The device picker excludes devices visible only through a post office.
  Connect both devices over a shared LAN or reachable VPN, then choose
  **Refresh devices**. A device becomes selectable when a direct route is
  discovered; linking still checks that the route works. If that route disappears,
  the device is removed from the picker and **Link** is disabled.
- Allow Mesh-Talk through the firewall. Guest Wi-Fi or client isolation can
  prevent device-to-device connections even on the same Wi-Fi network.
- If linking asks for an upgrade, update Mesh-Talk on both computers, then
  generate a fresh code. Sharing the same login name and password requires the
  current desktop linking protocol; changing history choices cannot bypass it.
- If linking times out, check the connection, then generate a fresh code.
  The failure appears beside **Link** with an optional **Technical details**
  disclosure. Keep your history choice; a network failure does not require
  enabling history synchronization.

## Choose what synchronizes

The linked account has one nickname, avatar and bio (personal signature).
When profile synchronization is enabled, linked devices apply the signed account
profile and other members see that account identity regardless of which device
sends a message. The joining device’s previous local nickname does not replace
the kept account’s profile. Subsequent account profile edits synchronize through
the same signed revision owner.

History, attachment contents, account profile, appearance and contact preferences
have separate category controls. The complete-sync option enables all five;
manual category edits retain explicit opt-outs. History includes available direct,
Message Yourself and authorized channel history. File cards and file bytes remain
separate: a card can arrive before the download finishes. Synchronization proceeds
while an authorized source is reachable, rather than completing every transfer
inside the pairing dialog.

**Waiting for files** means that message/account synchronization can proceed while
file contents are unavailable from the linked device. Pending file cards remain
visible, and retries resume automatically with a bounded interval. A card contains
metadata, not a recoverable copy of its bytes; a usable copy must remain on an
eligible source. Actual transfer or checksum failures are still reported as errors.
Explicit file-availability status requires current clients on both devices.

Supported account data follows these boundaries:

| Data | Linked-device behavior |
| --- | --- |
| Login name and password | One canonical account login; private credential application is independent of message-history progress |
| Nickname, avatar and bio | One signed account profile, subject to profile synchronization selection |
| Contact remarks and portable contact choices | Private account preferences; remarks never replace the contact's public nickname |
| Saved Messages and other conversations | Available retained events, subject to history selection and current conversation authority |
| Private DM reactions and recalls | Verified account controls under history selection; removed reactions and recalled messages retain their control records |
| Message Favorites | The account's bounded Favorites collection, including removal records; file availability is checked separately |
| Group membership and roles | Accepted account grants inherit to certified devices; legacy device-only groups require compatible migration |
| Draft text and protection choices, exact-message read state | Private versioned account documents under history selection; draft deletions retain tombstones |
| Portable settings and interface language | Per-key private preferences under appearance selection; incoming changes preserve unrelated local settings |
| Custom contact/group photos and installed customization libraries | Validated private account assets under appearance selection; deleting an asset retains its deletion record |

OS permissions, device identities, local encrypted-storage secrets and manual
verification records stay on their devices. The existing local user and its
retained history and files remain in place when sign-in switches to the shared
credentials. Passwords never appear in public profiles, contact remarks or
ordinary chat messages. Channel membership is still required for channel
history. Deleted or expired data cannot be recreated. Appearance settings share
theme and wallpaper selections together with validated custom library assets.
Private DM reactions and recalls synchronize through account records; channel
reactions follow the authorized group event stream. Both devices need a current
client for private DM controls. Kept-source preparation validates retained older
controls before publishing them; the joining device does not import its former
account’s controls. Locally deleting a conversation does not recall its messages
for everyone.

Open **Message Yourself → Devices** to see this device and the other devices linked to your account, including currently offline devices. The device list stays collapsed until you open it. Online status reflects current presence observations; it is separate from whether synchronization is enabled or complete. If no other devices are linked, you can choose **Link a device** from the list. Message Yourself remains available for local notes and files.

Use **Message Yourself** for notes and files addressed to your own account. You can save them before linking another device. Linked devices synchronize eligible retained content according to your selections when a trusted source is reachable.

Channel member lists show each account once, with its linked devices in the device
details. Device-bound ownership and invitation acceptance are unchanged. Removing
a person removes that account's certified channel devices together; transferring
ownership still selects a specific accepted device.

Saved Messages is an account conversation shared by linked devices. Its history
must be refreshed even when the current account is absent from the ordinary
contact list. A sent copy appearing on your other device proves sibling sync,
not delivery to an external contact: only a recipient confirmation completes
external delivery. Pending delivery remains **Awaiting delivery**.

A post-office bridge is not a substitute for the direct reachable path needed by device synchronization. Linking is not an encrypted backup, and it cannot recreate data that no device retains.

If an external message remains **Awaiting delivery** after linking, a device may
still be pinned to its former account at the recipient. The recipient can open
your member profile or verification dialog and review the old/new account IDs
and device fingerprint. After confirming the change with you, they can choose
**Accept this account change**. This is a recipient-side identity decision;
opening your profile or seeing a synchronized sent copy does not accept it.
See [peer identity and trust](../../peer-trust.md#recover-a-device-that-changed-accounts).

See [Backup and restore](backup.md) before replacing or losing access to a device.

## Further reference

[Linked-device architecture](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/ARCHITECTURE.md)
