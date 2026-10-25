# Back up and restore your account

Create an encrypted archive and keep its recovery key separately.

## Create an encrypted backup

1. Open **More → Settings → History & backup → Create encrypted backup**.
2. Choose **Google Drive** or **Local file** as the destination.
3. On first use, save the recovery key separately, such as in a password manager,
   and confirm that you saved it. This is a one-time setup.
4. Choose **Back up now**. For a local backup, choose a new `.mtbackup` file.
   Wait for the success message.

Later backups reuse the same key, protected in this local user's encrypted storage.
You can view it again in the backup dialog. Keep a separate copy for recovery after
losing your device. Older backups still need the original key used to create them.

## Use Google Drive

In a configured build, choose **Google Drive** as the backup destination and connect
through your browser once. After the one-time recovery-key setup, choose **Back up now**.
The app remembers the connection for later manual backups; reconnect if access is revoked.
To restore, choose **Google Drive** as the backup source on the signed-out recovery
screen, connect the same Google account, select a backup, enter its recovery key
and choose **Continue** to download and verify it.
Google sign-in does not replace the recovery key. Backups are manual and are stored
in Mesh-Talk's private Drive folder. Local-file backups remain available.

## Restore on a new installation

1. On the signed-out screen, choose **Restore from encrypted backup**.
2. Choose the archive and enter its matching recovery key.
3. Review the verified identity, date, content counts and any missing media.
4. Choose an unused local username and a new local password, confirm restore, then sign in.

The recovered installation keeps the key you entered encrypted locally for future
manual backups. Keep your separate copy for another recovery.

Restoring retains the account fingerprint but creates a new device fingerprint. It does not merge into an existing local profile or revoke a lost device. Future channel access can require the owner to invite the new device.

## Know what is excluded

Backups include Favorites, their titles and notes, and retained Favorite attachments, even if the original chat history was cleared. Expired content stays unavailable. File cards without retained bytes remain visible; the preview reports missing Favorite files separately. Backups made before Favorites backup support contain no Favorites.

Backups exclude machine settings, local UI customizations, pending deliveries, old device keys and files outside Mesh-Talk's managed stores. There is no vendor password-reset or recovery service. Losing both the backup and its key can make the old identity and encrypted history unrecoverable.

## Further reference

[Complete backup and recovery guide](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/backup-recovery.md)
