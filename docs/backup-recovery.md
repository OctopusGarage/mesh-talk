# Backup and recovery

Mesh-Talk has no account server. Your local password unlocks this installation;
it does not recover data from a lost device. Create a manual encrypted backup
while you still have access to an unlocked device.

## Create a backup

1. Open **Settings → History & backup → Create encrypted backup**.
2. Read the included and excluded data list. Save the displayed recovery key
   somewhere separate from the backup file, then enter the requested key groups.
3. Choose a new `.mtbackup` destination. Messaging pauses briefly while Mesh-Talk
   takes a consistent snapshot. Wait for **Backup saved and verified**.

Each export generates its own key. A later export does not change the key needed
for an older file. Mesh-Talk stores only the last successful backup's location,
time, size, and counts on this device; it never stores the recovery key.

## Restore after losing a device

1. On the signed-out screen, select **Restore from encrypted backup**.
2. Choose the `.mtbackup` file and enter its matching key. Mesh-Talk verifies the
   entire archive before showing the account fingerprint, backup date, content
   counts, and missing media count.
3. Choose a new local username and password. The username must be unused on this
   installation. Confirm restore, then sign in with those new credentials.

The recovered account fingerprint stays the same. A new device fingerprint is
created. Old device keys and live encryption ratchets are never copied. Channel
messages recovered from the old device are readable as archives; sending and
receiving future channel messages requires the channel owner to invite the new
device. Other members can supply old encrypted events, but the new device
cannot decrypt channel messages missing from its backup. File cards without
locally managed bytes remain visible but their contents may be unavailable.
Some direct messages after the backup date may return through peer sync.

The backup includes visible message history, received peer avatars, file
cards, and locally managed media referenced by those cards. It excludes local UI
customizations (including your own avatar override), machine settings, pending
deliveries, old device keys, and files outside Mesh-Talk's media store. It never
merges a backup into an existing local profile.

If an old device may be in someone else's hands, restoring the backup does **not**
revoke it. Choose **Start with a new identity** instead and ask contacts to verify
the new fingerprint. If both the backup and its key are lost, the encrypted history
and previous identity cannot be recovered by a password reset.
