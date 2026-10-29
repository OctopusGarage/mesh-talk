# Settings

Settings opens on **App behavior**. Choose a category to display its page; only that
page is mounted, and scrolling never changes the selected category. Routine
changes save automatically after the backend acknowledges them. The status line
shows saving and successful completion; failures appear with the affected
category. If a write succeeds but reloading its canonical value fails, reload
settings to confirm the value before editing it again.

## Categories

Categories follow three navigation groups. Narrow windows retain the same order in
an accessible horizontal navigation strip.

| Group | Category | Controls |
| --- | --- | --- |
| Experience | App behavior | Launch at OS login and close-to-tray behavior. |
| Experience | Appearance & language | Display language, themes, theme wallpapers and wallpaper visibility. |
| Experience | Notifications & calls | Notifications, message previews, experimental calling and ringtone. |
| Experience | Shortcuts | Screenshot bindings, opt-in global registration, registration status and reset. |
| Privacy & data | Privacy & security | Privacy preset, lock/sign-in, clipboard protection, hidden contacts and related permission summaries. |
| Privacy & data | Files & downloads | Attachment receiving defaults and download destination; conversation-specific overrides remain available. |
| Privacy & data | History & backup | Local retention, encrypted backup, last-backup metadata and restore guidance. |
| Connection & extensions | Network & delivery | Visibility/reply permissions, device and cross-network discovery, offline delivery and preferred post office. |
| Connection & extensions | Agent access | Scoped local agent access, allowed conversations, grant duration, activity and setup instructions. |

Privacy has separate preset, locking/sign-in and clipboard sections. Notification
previews and agent grants retain one authoritative editing page, with read-only
summaries and links from privacy. Network controls distinguish device discovery,
cross-network discovery and offline delivery; dependencies and saved preferences
retain their existing behavior.

Internal category IDs and stored preference keys remain stable; `shortcuts` is the
new category. `settings/model.ts` owns groups, order and control-to-category routes.
Moved control links resolve through that mapping, including language, screenshots
and agent access. Save/error feedback belongs to the page that contains the editor.

## Linked-account scope

With preference synchronization enabled, linked devices share portable settings:
notifications and preview preference, retention, calling and ringtone, post-office
master/send/receive, LAN restriction, bridge discovery/hub, privacy preset, idle and
hidden locking, sensitive-copy confirmation and clipboard clearing. Existing
appearance preferences include language, theme, wallpaper and library selection.
Custom themes, stickers, avatars and their validated assets use versioned private
account documents. Contact remarks, pins and private contact/group photos share
account scope; they do not change a contact's public signed nickname.

Download destinations, autostart, close-to-tray, stay-signed-in, update channel,
OS authorization, local-agent grants, screenshot shortcuts and device sync selection remain local.

Imported linked-device preferences may strengthen local privacy protections,
but do not silently weaken them. Weaker imported preview, lock, clipboard,
copy-confirmation or retention values retain the current local protection;
mixed settings show Custom. An explicit local change can apply the weaker value
after [identity verification](privacy-controls.md#verify-sensitive-operations).
Invisible mode and reply-permission grants remain device-local under
[privacy controls](privacy-controls.md). Attachment acceptance is a decision by
the receiving device, and TOFU/manual safety verification remains installation-
local; neither is expanded by importing another device’s settings.
Enabling a shared notification preference never grants OS permission implicitly.
Local encrypted-storage keys and device identity also remain distinct.

The explicit kept-source pairing action seeds missing portable preferences and
legacy libraries, drafts and private photos. Storage performs create-only seeding
atomically, preserving existing values and tombstones. Fresh joining-device defaults
cannot overwrite established account choices. Each preference key has an independent
version; changes overlay the current canonical settings instead of replacing an
outdated whole-device snapshot. Backup does not export device-local settings;
see [backup and recovery](backup-recovery.md).

## Local agent access

The conversation selector shows selected conversations out of the listed total.
The separate scope hint states the authorization limit of 16 conversations.

In **Agent access**, expand **Allowed conversations** to choose the contacts or joined
groups a local agent may send to, then enable **Local agent access**. The collapsed
picker shows the selected count out of the listed total. Search contacts and groups or use
**Selected only** to review and remove grants, including unavailable conversations.
The list scrolls within a bounded area so large rosters do not grow the settings
page. Search and filters never change saved permissions. Selecting or removing a
conversation saves immediately; removing the final selection disables access.

Privacy & security also shows the agent's acknowledged status and selected
conversation count, with a link to this editor. Enabling access or expanding its
scope requires [fresh identity verification](privacy-controls.md#verify-sensitive-operations).
Disabling access and removing selected conversations remain available without
an additional verification prompt.

New authorizations default to **One hour**. **Until I turn it off** is an explicit
choice; existing continuous authorizations remain continuous. The page shows
the saved deadline and an expired state. **Renew authorization** requires identity
verification; refreshing status, retrying a failed service, editing the selected
conversations and restarting do not renew authorization. Recent activity shows
content-free accepted/retried sends from the local agent interface; it does not
identify a particular AI application or prove recipient delivery.

The switch starts the desktop's existing
integration service with a loopback HTTP adapter. No CLI or additional download
is needed. Agents can send text and replies; they cannot read chat history.

**Copy instructions** is available directly under **Connect your agent** after
access is enabled. **View agent instructions** expands the same short bootstrap
containing the live API help URL and the managed-file explanation. Technical
paths remain inside this disclosure. Enabling also maintains a single
Mesh-Talk block in the displayed Codex user-level `AGENTS.md`. Disabling stops
admission, removes the scoped credential and removes only that block, preserving
other instructions. Saved choices belong to the signed-in account on this device.
Running, disabled and unavailable states are reported separately, with retry for
failures. The app must remain running and unlocked; normal login restores enabled
access and updates the bootstrap to its new endpoint. See the
[local API guide](bot-integrations.md#desktop-local-agent-access) for discovery,
authority, credential handling and transport limits.

## Screenshot shortcuts

**Shortcuts** provides separate bindings for **Capture screen** and **Hide window &
capture**. Click a binding and press a modifier with a letter, digit or F1–F12;
Esc cancels recording without closing Settings. Clear removes one binding.
**Restore defaults** restores both bindings while preserving the global switch.
The defaults are Ctrl+Shift+X and Ctrl+Alt+Shift+X on Windows/Linux, and
Command+Shift+X and Command+Option+Shift+X on macOS. Editing/system reservations
and duplicate bindings are rejected. Shortcuts save with the signed-in local
user's device preferences, survive restart, and never propagate to linked devices.

**Use screenshot shortcuts globally** is off by default. Application shortcuts
work while Mesh-Talk is focused, including from the composer. Opting in uses the
same bindings while another application is active or Mesh-Talk is in the tray.
The app must be running, signed in and unlocked. Linux global registration
requires a native X11 session; Wayland retains application shortcuts. Native OS
registration failures are reported with Retry; a failed replacement preserves
the previous saved bindings and restores registrations where the OS permits.
Unexpected rollback failures report degraded global availability instead of
claiming that a partially restored set is active.

Background activation acquires the screen before showing the editor. It uses
the existing capture backend and display selection; it does not change the
platform's screen-recording permissions. With no writable conversation selected,
the shortcut only shows Mesh-Talk and asks the user to choose a conversation.
An open modal, shortcut recorder, active capture or editor blocks another capture.
IME composition and held-key repetition never launch capture. Screenshots retain
their originating conversation and enter the existing pending-attachment flow;
only Send/Enter from that flow hands them to delivery.

Composer tooltips show the acknowledged binding and effective scope (App only or
Global). The editor's floating controls expose their actual shortcuts, including
native modifier labels and accessible `aria-keyshortcuts`. See the
[screenshot guide](help/topics/screenshots.md) for editor keys.

`src-tauri/src/screenshot_shortcuts.rs` owns canonical validation, transactional OS
registration, native session/lock checks and registration tokens. Renderer
cleanup cannot suspend a newer registration. Runtime replacement, locking and
logout retire listeners and registrations; capture checks session and lock
revision again before releasing pixels or restoring a window. The frontend
`screenshot/useScreenshotShortcuts.ts` connects this owner to attachment intake,
and `shortcutRouter.ts` admits at most one acquisition independently of native
event-listener setup. The ordinary settings controller owns acknowledged edits;
a successful shortcut write updates routing even if full-settings readback fails.

Focused native and frontend tests cover validation, rollback, local persistence,
event routing, stale generations, delayed cleanup, modal/input ownership and
save/readback failures. Browser scenarios exercise settings at 1440×1000 and
760×520, customization, hints and synthetic global events. Browser mocks do not
prove OS accelerator delivery, tray wakeup, permission dialogs or native window
restoration. Those remain packaged native acceptance boundaries on each platform.

## System notification permission

The notification switch saves the signed-in account's shared preference;
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
hides guidance without changing the saved preview preference. Messages sent to
yourself do not trigger a reply-notification reminder.

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

## Message Yourself and linked devices

**Message Yourself** is always available after the account is ready, including when
no peers are online. Text, stickers and file cards use the account's ordinary
conversation history. Acceptance saves them locally; it does not promise that
another device has received them. Personal messages do not expire automatically.

In **Link a device**, **Message history** controls sharing and receiving available
messages and file information with certified devices on your account.
**Attachments** independently permits sharing and downloading the available file
bytes. Both endpoints must permit the relevant category. Turning these off does
not disable conversations with other people or groups.

The group directory remains available across linked devices independently of
these optional data switches. It contains descriptive group and member facts,
including references to legacy groups on another device; it contains no message
bodies or sending keys and does not grant access. A reference remains read-only
until the device obtains the group's validated state.

Upgraded devices can synchronize file cards without downloading their contents.
Older devices retain their existing attachment-coupled metadata behavior; the
linked-device status explains when an upgrade is needed. Choosing no history
before linking requires a source device supporting that choice; an older source
must be upgraded rather than transferring history first.

Synchronization resumes when a trusted source is reachable and still retains the
selected data. Local retention and deletion can remove that source; this feature
is not an unlimited cloud archive. A local save remains useful while offline.

## Favorites and linked-device permissions

**Favorites** is separate from the self-chat **Message Yourself**. It stores useful
message snapshots with editable searchable descriptions and per-message notes.
Both linked devices must enable **Message history** to exchange Favorites,
including notes and removals, and **Attachments** to exchange retained file bytes.
Disabling sharing preserves local favorites. See [Favorites](favorites.md) for
retention, compatibility and file-size limits.

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

Last-backup status is available without preparing a recovery key. First-time
backup setup asks the user to save and acknowledge one reusable recovery key;
later exports use the encrypted saved key without a typing challenge. The selected
destination and signed-in Google connection are remembered for manual backups. The
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
changing the saved value. Opening Settings normally still starts at App behavior.
See the [user guide](help/README.md).
