# Back up and restore your account

Create an encrypted archive and keep its recovery key separately.

## Create an encrypted backup

1. Open **More → Settings → History & backup → Create encrypted backup**.
2. Read what is included and excluded.
3. Save the new recovery key separately from the backup file and complete the key confirmation.
4. Choose a new `.mtbackup` destination and wait for **Backup saved and verified**.

Each backup has its own recovery key. The app does not save that key, and an old backup still needs its original key.

## Restore on a new installation

1. On the signed-out screen, choose **Restore from encrypted backup**.
2. Choose the archive and enter its matching recovery key.
3. Review the verified identity, date, content counts and any missing media.
4. Choose an unused local username and a new local password, confirm restore, then sign in.

Restoring retains the account fingerprint but creates a new device fingerprint. It does not merge into an existing local profile or revoke a lost device. Future channel access can require the owner to invite the new device.

## Know what is excluded

Backups exclude Favorites and their retained attachments, machine settings, local UI customizations, pending deliveries, old device keys and files outside the managed media store. There is no vendor password-reset or recovery service. Losing both the backup and its key can make the old identity and encrypted history unrecoverable.

## Further reference

[Complete backup and recovery guide](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/backup-recovery.md)
