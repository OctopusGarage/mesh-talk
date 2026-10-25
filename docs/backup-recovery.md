# Backup and recovery

Mesh-Talk has no account server. Your local password unlocks this installation;
it does not recover data from a lost device. Create an encrypted backup
while you still have access to an unlocked device. Backups run only when you choose
**Back up now**; there is no background schedule. Save it locally or, in a
configured build, upload it directly to your Google Drive.

For first-use sign-in, saved credentials and the optional account-protection
reminder, see [Keep access to your account](help/topics/first-run.md#keep-access-to-your-account).

## Create a backup

1. Open **Settings → History & backup → Create encrypted backup** and choose
   **Google Drive** or **Local file** as the destination.
2. During first-time setup, save the displayed recovery key somewhere separate
   from your backups, such as a password manager. Confirm **I saved my recovery key**.
   You will need this key if you lose this installation; Google sign-in cannot
   decrypt your history.
3. Choose **Back up now**. For a local file, choose a new `.mtbackup` destination.
   Messaging pauses briefly for a consistent snapshot, then restarts. Wait for
   the success message before closing the app.

Later backups reuse the saved recovery key. Mesh-Talk remembers your destination
and protects the key in this local user's encrypted storage, unlocked by your local
password. You do not need to generate a key or type key groups for each backup.
You can view the saved key again from the backup dialog. Keep your separate copy:
the locally saved key cannot help if the device or local profile is lost.

Older backups made with separate per-export keys still need their original keys;
setting up a reusable key does not reencrypt existing archives. The last successful
backup's location, time, size and counts are also stored encrypted on this device.

## Restore after losing a device

1. On the signed-out screen, select **Restore from encrypted backup**.
2. Choose the `.mtbackup` file and enter its matching key. Mesh-Talk verifies the
   entire archive before showing the account fingerprint, backup date, content
   counts, and missing media count.
3. Choose a new local username and password. The username must be unused on this
   installation. Confirm restore, then sign in with those new credentials.

A successful restore retains the recovery key you supplied in the new local user's
encrypted storage, so later backups do not ask you to set up another key.

The recovered account fingerprint stays the same. A new device fingerprint is
created. Old device keys and live encryption ratchets are never copied. Channel
messages recovered from the old device are readable as archives; sending and
receiving future channel messages requires the channel owner to invite the new
device. Other members can supply old encrypted events, but the new device
cannot decrypt channel messages missing from its backup. File cards without
locally managed bytes remain visible but their contents may be unavailable.
Some direct messages after the backup date may return through peer sync.

The backup includes visible message history, received peer avatars, file
cards, locally managed media referenced by those cards, and Favorites with their
titles, descriptions, message notes and independently retained attachments. A
favorite remains recoverable after its source chat history has been cleared.
Unavailable attachments remain file cards; the preview reports missing ordinary
media and missing Favorite files separately. It excludes local UI
customizations (including your own avatar override), machine settings, pending
deliveries, old device keys, and files outside Mesh-Talk's media store. It never
merges a backup into an existing local profile.

If an old device may be in someone else's hands, restoring the backup does **not**
revoke it. Choose **Start with a new identity** instead and ask contacts to verify
the new fingerprint. If both the backup and its key are lost, the encrypted history
and previous identity cannot be recovered by a password reset.

## Snapshot compatibility

New backups use snapshot schema 5 and retain the released schemas 1–3 record tags.
Schema 5 combines logical Favorites and independently retained attachments with
signed account profiles and a required immutable account-creation metadata record.
The restored key and creation metadata are installed together before the account
opens. A backup creation date never becomes an account creation date.

The reader also accepts both earlier development schema-4 formats: Favorites-only
and account-profile/creation metadata. Their colliding extension tags are decoded
strictly with bounded exact encodings; a snapshot cannot mix the two families.
Missing authoritative creation metadata remains unknown. Schema 5 keeps Favorites
at tags 12–14 and appends creation metadata and profiles at tags 15–16; new exports
always use schema 5. The outer authenticated archive format is unchanged.

Favorites preserve removal tombstones and expiry restrictions. Only already
retained, live referenced files are exported, with the existing 64 MiB per-file
limit. Canonical-known protected Favorites are excluded as a single eligibility
projection for records, counts and attachments; ordinary Favorites remain portable
after their original history is cleared. Ordinary and Favorite media share the
8 GiB aggregate media limit. Size, chunk order and SHA-256 are checked before
publication. Staged attachment files open only while writing or installing, so
large collections do not require one handle per retained file.

Schema 3 signed public trust cards and proofs remain compatible, but manually
declared dates never become system-recorded creation metadata. Private relationship
observations are not included in public profile records. The retired
message-language metadata in older schema 2 backups is validated for format and
authenticity but does not restore restrictions or hidden-message decisions.
New backups omit that metadata. Display-language preferences remain ordinary
local device settings.

## Google Drive backup and restore

Google Drive is an optional destination for the same encrypted `.mtbackup` format.
Backups run only when requested; there is no schedule or background cloud sync.

1. Open **Settings → History & backup → Create encrypted backup**.
2. Set **Backup destination** to **Google Drive**, then **Connect Google Drive**.
   Choose the intended Google account in your system browser and grant access.
   Mesh-Talk remembers the connection for this local user.
3. If this is your first backup, save the recovery key and acknowledge it once.
4. Choose **Back up now**. Messaging pauses only for the consistent local snapshot,
   then restarts while the encrypted file uploads. Wait for the cloud success message.

For later backups, open the dialog and choose **Back up now**. Mesh-Talk restores
its saved Google connection without opening the browser. Expired or revoked access
requires reconnecting; temporary network errors can be retried. Disconnecting or
changing Google accounts explicitly forgets the remembered connection on this
installation. Merely closing the dialog or cancelling a transfer retains it.

To restore on another installation, open **Restore from encrypted backup** on the
signed-out screen and set **Backup source** to **Google Drive**. Connect the same
Google account, select an archive, enter its recovery key and choose **Continue**.
Mesh-Talk downloads and authenticates the archive before showing its preview.
Review the preview and explicitly confirm restoration
with an unused local username and new password. The existing account/device and
channel-recovery rules above apply unchanged. Cloud listing names and sizes are
provider metadata; account identity and content counts are trusted only after
archive authentication.

**Cancel transfer** stops cloud work and retires its temporary operation connection. If a local
snapshot is already running, it finishes safely and restarts messaging before the
cancellation returns. Confirmed local profile restoration completes transactionally;
it is not interrupted during publication. Reconnect to retry or change Google accounts.
Closing the dialog retires its active operation connection, while the encrypted
remembered grant remains available for later manual backups. Local-file export and restore stay available
when Google cannot be reached or the build has no Google configuration.

### Google Drive security and boundaries

- The desktop host requests only `drive.appdata`. Archives live in the hidden
  `appDataFolder`, accessible through Mesh-Talk rather than the Drive file browser.
- Google receives encrypted archive bytes, archive names, sizes and ordinary service
  request metadata. Recovery keys and local passwords are never sent to Google.
  Each archive needs the recovery key used when it was created. Google sign-in alone cannot
  recover an account or decrypt history.
- OAuth authorization uses the system browser, PKCE, an unpredictable state and a
  temporary IPv4 loopback callback. The native host remembers the refresh token
  in this local user's password-encrypted storage and refreshes expiring access
  tokens without another browser sign-in. Tokens never reach frontend settings,
  localStorage or recovery archives. Signed-out restore connections are temporary
  and never borrow a signed-in user's saved grant. Explicitly disconnecting forgets
  local remembered access; it does not revoke consent in Google Account settings.
- Connections are tied to the exact desktop session generation, including signed-out
  restore sessions. Logout, lock/session replacement and view retirement invalidate
  further work. The saved grant may be reused only after signing in and unlocking
  the same local profile. Work already admitted may finish; it cannot publish into a replacement
  profile. Temporary encrypted files are removed when their owning operation retires.
- Uploads create new archive files. A remote archive becomes selectable only after
  its size and SHA-256 checksum match the locally verified encrypted archive.
  Downloaded size and checksum are checked before existing cryptographic verification.
  Transfers use bounded chunks, fixed HTTPS destinations, disabled redirects and
  request timeouts. The transfer ceiling is 9 GiB including archive framing; the
  core's existing 8 GiB plaintext limit remains authoritative.
- Previous backups are retained. Interrupted or unverified uploads are never offered
  for restoration. Drive storage limits and user deletion still apply. Clearing
  Mesh-Talk application data in Drive deletes its cloud backups; retain local copies
  and keep recovery keys separately.

### Configure a desktop build

Create a Google Cloud project, enable the Google Drive API, configure the OAuth
consent screen with the `https://www.googleapis.com/auth/drive.appdata` scope and
register an OAuth client of type **Desktop app**. During Google OAuth testing, add
the Google accounts that will test the build to the consent screen's test users.
Follow Google's publication/verification requirements before distributing broadly.

Build the desktop app with `MESH_TALK_GOOGLE_CLIENT_ID` set to the registered desktop
client ID. If Google supplies/requires its desktop client secret, also set
`MESH_TALK_GOOGLE_CLIENT_SECRET` at build time. Native application client credentials
are distributed with the app and are not confidential server credentials; PKCE
protects each authorization exchange. Never use a Web application client's secret.
These variables affect compilation; restart/rebuild the native host after changing them.
No vendor-operated OAuth callback server is required. Release jobs must receive the
same registered desktop client configuration to enable this optional capability.
Unconfigured builds explain that Google Drive is unavailable and retain local backups.

Provider references: [desktop OAuth](https://developers.google.com/identity/protocols/oauth2/native-app),
[Drive application data](https://developers.google.com/workspace/drive/api/guides/appdata).

Native acceptance requires a configured packaged app on supported platforms: prove
browser callback/denial/timeout, account selection, cancel during snapshot and upload,
offline and revoked access, unchanged previous backups and fresh-install restore using
the saved recovery key. Unit HTTP fixtures and browser IPC mocks cover application
boundaries but do not prove Google's live consent and transfer behavior.
