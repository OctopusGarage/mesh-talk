# Use your identity on another device

Link devices through Your devices so they share an account after local setup.

## Open device linking

1. On the second computer, create a local profile and sign in first. This gives you access to its device-linking controls; profile creation alone does not join your existing account.
2. On the original signed-in device, open **More → Link a device** and choose **Show pairing code**.
3. On the second computer, open **More → Link a device**, select the original device and enter its code. Linking makes the second device share the original device's account.
4. Keep the devices on a reachable path and complete the linking flow. Review the account identity, linked devices and synchronization categories afterward.

Treat a linking code as sensitive. Do not share it with someone who should not join your account.

## Choose what synchronizes

The device dialog shows available synchronization categories and status. Choose history, file cards and attachment contents separately. History shares available direct, accepted-channel and Saved Messages history when permitted. A synchronized file card does not mean its contents are downloaded. Appearance can participate when its category is enabled; machine-specific and privacy choices do not all synchronize.

Open **Saved Messages → Devices** to see this device and the other devices linked to your account, including currently offline devices. The device list stays collapsed until you open it. Online status reflects current presence observations; it is separate from whether synchronization is enabled or complete. If no other devices are linked, you can choose **Link a device** from the list. Saved Messages remains available for local notes and files.

Use **Saved Messages** for notes and files addressed to your own account. You can save them before linking another device. Linked devices synchronize eligible retained content according to your selections when a trusted source is reachable.

A post-office bridge is not a substitute for the direct reachable path needed by device synchronization. Linking is not an encrypted backup, and it cannot recreate data that no device retains.

See [Backup and restore](backup.md) before replacing or losing access to a device.

## Further reference

[Linked-device architecture](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/ARCHITECTURE.md)
