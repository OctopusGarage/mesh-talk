# Settings

Settings opens on **General**. Choose a category to display its page; only that
page is mounted, and scrolling never changes the selected category. Routine
changes save automatically after the backend acknowledges them. The status line
shows saving and successful completion; failures appear with the affected
category. If a write succeeds but reloading its canonical value fails, reload
settings to confirm the value before editing it again.

## Categories

| Category              | Controls                                                                                                   |
| --------------------- | ---------------------------------------------------------------------------------------------------------- |
| General               | Display language, launch at OS login, close to tray and scoped local agent access.                                 |
| Appearance            | Theme/library, available theme wallpapers and wallpaper visibility.                                        |
| Notifications & calls | Notifications, message previews, experimental calling and ringtone.                                        |
| Connection & delivery | Invisible mode, reply permissions, discovery scope, relay master/send/receive and preferred post office.   |
| Privacy & security    | Device privacy preset, idle/hidden locking, sensitive copy/clipboard, stay signed in and hidden contacts.  |
| Files & receiving     | Attachment receiving default and download destination. Individual conversations retain their own override. |
| History & backup      | Local retention, encrypted backup, last-backup metadata and restore guidance.                              |

App startup and keychain sign-in are computer-wide choices; most communication
preferences belong to the signed-in account on this device. Appearance can
participate in linked-device synchronization when its category is enabled. Each
page names its scope, with exceptions beside the affected control. Backup does
not export the device's settings; see [backup and recovery](backup-recovery.md).

Before issuing a pairing code, only explicitly saved local appearance choices
seed missing account preferences. Fresh-device defaults remain local, and
existing account values are preserved.

## Local agent access

In **General**, choose the contacts or joined groups a local agent may send to,
then enable **Local agent access**. The switch starts the desktop's existing
integration service with a loopback HTTP adapter. No CLI or additional download
is needed. Agents can send text and replies; they cannot read chat history.

**View agent instructions** expands a short bootstrap containing the live API help
URL; **Copy instructions** copies the same text. Enabling also maintains a single
Mesh-Talk block in the displayed Codex user-level `AGENTS.md`. Disabling stops
admission, removes the scoped credential and removes only that block, preserving
other instructions. Saved choices belong to the signed-in account on this device.
Running, disabled and unavailable states are reported separately, with retry for
failures. The app must remain running and unlocked; normal login restores enabled
access and updates the bootstrap to its new endpoint. See the
[local API guide](bot-integrations.md#desktop-local-agent-access) for discovery,
authority, credential handling and transport limits.

## System notification permission

The notification switch saves the signed-in account's preference on this device;
it does not override operating-system authorization. While enabled, its row shows
current system permission separately. Turning the switch on checks permission and
requests access only when the OS has not recorded a decision. Existing denials
show **Open system settings**, manual instructions and **Check again**. Opening
settings always requires a click; failures retain the manual recovery path.

Permission is observed when notifications are enabled, when opening their settings
page, on window focus/visibility restoration, and after submitting an outgoing
message. Concurrent observations share pending work but settled results are never
used as permanent authorization. Startup and passive observations do not prompt.
Closing the originating view or replacing its account invalidates a request that
has not yet reached native prompting; a system dialog already started cannot be
recalled by closing the view.

After submitting text, a sticker or a file to another person or group, blocked,
undecided or limited permission produces a dismissible notice below the
conversation header. Message admission and delivery never await permission. A
stalled permission check cannot hold the composer; incoming history and updates
to an existing outgoing intent do not trigger the reminder. Dismissal lasts for
the login generation, including conversation changes. Disabling notifications
hides guidance without changing the saved preview preference. Saved Messages do
not trigger a reply-notification reminder.

macOS checks the installed application's UserNotifications authorization and alert
settings; quiet/provisional access is shown as limited. Windows checks the
installed application's ToastNotifier setting using the same app identifier as
notification delivery. Native failures, uninstalled development binaries and
unsupported desktop queries report **couldn't be verified**, never an assumed
grant or denial. Linux has no portable per-application permission query or
notification-settings URI, so the row provides manual desktop instructions.
OS authorization does not prove an alert appeared: Focus, Do Not Disturb, sound,
badge and presentation choices may independently suppress it.

The native module `src-tauri/src/notifications.rs` owns OS checks, explicit
permission requests and fixed settings destinations. The typed notification IPC
adapter and `frontend/src/lib/notify.ts` share observations; the settings controller
remains the owner of saved preferences. Focused unit and browser tests cover
state transitions and recovery. Real packaged-app permission dialogs, settings
navigation and notification presentation still require native platform evidence;
browser mocks do not certify those boundaries.

## Saved Messages and linked devices

**Saved Messages** is always available after the account is ready, including when
no peers are online. Text, stickers and file cards use the account's ordinary
conversation history. Acceptance saves them locally; it does not promise that
another device has received them. Personal messages do not expire automatically.

In **Link a device**, **Message history** controls sharing and receiving available
messages and file information with certified devices on your account.
**Attachments** independently permits sharing and downloading the available file
bytes. Both endpoints must permit the relevant category. Turning these off does
not disable conversations with other people or groups.

Upgraded devices can synchronize file cards without downloading their contents.
Older devices retain their existing attachment-coupled metadata behavior; the
linked-device status explains when an upgrade is needed. Choosing no history
before linking requires a source device supporting that choice; an older source
must be upgraded rather than transferring history first.

Synchronization resumes when a trusted source is reachable and still retains the
selected data. Local retention and deletion can remove that source; this feature
is not an unlimited cloud archive. A local save remains useful while offline.

## Network preferences and effective behavior

Saved preferences and current permission are separate facts. Disabling post
offices retains send/receive, discovery and preferred-relay choices. Dependent
controls are disabled with the reason shown; re-enabling the parent restores the
saved choices rather than resetting them.

- **Device discovery scope** offers **Local network only** and **Local and routed
  networks**. Local-only discovery uses recognized physical Ethernet/Wi-Fi
  interfaces and suppresses routed public discovery and post-office directories.
  Routed discovery also uses optional local Tailscale and EasyTier inventories;
  it does not enable post-office discovery. **Add device address…** opens the
  existing manual-address connection flow for other networks or unavailable
  clients. Existing authorized connections remain governed by their permissions.
  The saved `physical_lan_only` value and default are unchanged, so older settings
  retain their meaning. This control is not a connection firewall.
- **Invisible mode** suppresses public disclosure. Public bridge discovery stays
  inactive while invisible, even when its saved preference is enabled. A privacy
  policy that is loading/unavailable is not assumed to permit public disclosure.
- **Send off / receive on** is a partial relay mode: file consent and delivery
  confirmations need return controls. Direct peer transfers remain available.
- **Preferred post office** is a stored preference among known direct candidates.
  It is not a statement that the relay is currently connected or carrying traffic.
  Alternatives still need eligibility and a surviving route for fallback.

See the [network architecture](network-design-reference.md#4-settings-and-deployment-configuration)
for the complete protocol boundary. Visibility, content admission and physical
reachability remain distinct.

## Device privacy

Choose a preset in **Privacy & security**, read its effect description, then
choose **Apply preset**. The backend supplies the resulting canonical values;
the UI does not implement a second preset normalization policy. The preset
changes message previews, idle lock, hidden-window lock, sensitive-copy
confirmation and clipboard clearing. It does not change network visibility,
relay permissions or retention.

Notification preview has one editor in **Notifications & calls**. Its current
value and a link to that editor appear in Privacy & security. Individual changes
select the derived **Custom** state. Notifications-off keeps the saved preview
preference while making it inactive. See [privacy controls](privacy-controls.md).

## Receiving and saving files

Receiving approval and destination are separate decisions. The receiving default
chooses whether attachment transfers need consent. A conversation's attachment
settings can override that default. The download destination chooses where files
are saved; it does not grant transfer consent. After an accepted attachment has
finished transferring, the desktop saves it automatically to the configured
folder (or the system Downloads folder). Completed file cards reveal the local
copy; a failed write provides a retry action.

An unavailable receiving policy displays an unknown state with Retry. It does
not show an assumed Ask/Automatic choice as the persisted setting.

## Retention and backup

Shortening retention means changing Forever to a finite window, or reducing an
existing finite window. The confirmation states that eligible local history is
deleted immediately; increasing the limit later does not restore that history.
Cancel leaves the old value. **Create a backup first** opens the existing backup
workflow without saving the shorter retention window. Increasing retention or
selecting Forever does not require a destructive-change confirmation.

Last-backup status is available without generating a new recovery key. Export
still requires a fresh key and the existing key-confirmation challenge. The
native destination picker is guarded against duplicate requests and stale
owner/view completions. Restoration starts on the sign-in screen, as described
in [backup and recovery](backup-recovery.md).

## Implementation ownership and verification

`frontend/src/features/chat/SettingsDialog.tsx` owns dialog composition and
navigation. Its `settings/` directory owns category pages, shared accessible
rows/error presentation, a pure permission/retention model, and the controller.
The controller uses `settingsDialogSession.ts` for owner/view validity and the
IPC adapter's serialized read-modify-write queue. Preset effects and durable
settings remain authoritative in the host.

Attachment policy and backup retain their own domain-specific authorization and
lifecycle guards; shared presentation does not merge their persistence models.
The old anchor-scroll module was removed. See [module ownership](module-boundaries.md).

Focused coverage includes category/navigation consistency at 1440×1000 and the
760×520 supported minimum, retention cancellation/confirmation, shared preset
state, relay dependency combinations, failed reads/writes, owner-generation ABA,
native picker cancellation/rejection and recovery challenge gating. These browser
checks use mocked native IPC; they do not certify native OS dialogs, screen
readers, or physical-network delivery. Full-suite gates remain separate.

## Help navigation

**More → Help** links task guides to the relevant settings category. An explicit
help action can select a category and focus its control after loading, without
changing the saved value. Opening Settings normally still starts at General.
See the [user guide](help/README.md).
