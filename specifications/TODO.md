# Implementation status and acceptance boundaries

This stable path is the current status index, not a chronological TODO list or a
claim that every topology/platform combination has been verified. Feature and
protocol details belong to the linked owning guides; Git history records completed
work.

## Current capability areas

| Area                       | Implemented behavior                                                                                                                             | Reference / boundary                                                                                                                                                                    |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Direct conversations       | Signed discovery, identity-pinned Noise/TCP, encrypted account-addressed DMs, replies, reactions, mentions, search and retained history.         | [Architecture](../docs/ARCHITECTURE.md); discovery and TCP reachability remain separate.                                                                                                |
| Groups                     | Signed membership controls, sender-key encryption, bounded fanout and authorized member recovery with per-member sharing.                        | [Group flows and scenarios](../docs/network-design-reference.md#d-group-creation-distribution-and-member-recovery); a removed/nonmember device cannot acquire authority from discovery. |
| Relay networking           | Equal trusted post offices, shared public enrollment, opaque custody/federation, direct-first attempts, soft preference and alternatives.        | [Network baseline](../docs/network-design-reference.md); an alternate needs a surviving onward route.                                                                                   |
| Network controls           | Per-device master/send/receive/directory preferences, physical-LAN-only discovery and direct LAN/Tailscale relay selection.                      | [Privacy controls](../docs/privacy-controls.md); visibility does not grant conversation authority or create a firewall.                                                                 |
| Files and folders          | Encrypted chunks, exact-offer consent, durable decisions, restart recovery, integrity checks and recipient completion.                           | [File flow](../docs/network-design-reference.md#e-files-folders-consent-and-completion); card arrival is not byte completion.                                                           |
| Linked devices             | Code-authorized enrollment, certified sibling synchronization and device-local category preferences.                                             | [Pairing and replay](../docs/network-design-reference.md#g-linking-and-ongoing-account-synchronization); full replay needs its own live authenticated route.                            |
| Calls                      | Optional device-addressed live Noise signaling and separate WebRTC media.                                                                        | No post-office signaling fallback or TURN service; native WebView and physical capture acceptance remain separate.                                                                      |
| Desktop presentation       | Shared person/relay classification, account/device profile details, themes, avatars, stickers, virtualized navigation and default/lite editions. | [Visual system](../frontend/DESIGN.md) · [Customization packs](../docs/CUSTOMIZATION_PACKS.md).                                                                                         |
| Local privacy and recovery | Privacy presets, lock controls, hidden contacts, local retention, recall and encrypted manual backup/restore.                                    | [Privacy](../docs/privacy-controls.md) · [Backup/recovery](../docs/backup-recovery.md); restore creates a new device and cannot revoke an old one.                                      |

## Deliberate design boundaries

- **Pairing authorization:** a one-time 128-bit code binds the linker and joiner
  device keys. Durable enrollment invalidates old sessions; interrupted adoption
  has a restricted code-authenticated retry. There is no separate SAS ceremony.
- **Relay metadata:** production relays store opaque outer events, not readable
  original authors, conversation IDs or channel controls. Connection identities,
  pseudonymous mailboxes, size buckets and timing remain observable. See
  [the exact privacy boundary](../docs/post-office-metadata-privacy.md).
- **Bounded custody:** relay copies have capacity limits and a 30-day maximum age;
  this is not guaranteed minimum retention or permanent storage.
- **Connections:** the client Node has bounded sequential session reuse, not one
  multiplexed socket per relay or an eight-socket ceiling for the whole machine.
- **Reachability and trust:** Tailscale does not merge LANs; explicit relay trust
  and client directory consent are independent of routing.
- **Membership presentation:** removed group members' past reactions no longer
  render under the current-membership visibility rule.
- **Linux dependency constraint:** the maintained
  [GLib compatibility workflow](../.github/workflows/glib-0.20-watch.yml) tracks the
  GTK/Tauri dependency gate. Consult the lockfile and security configuration before
  changing versions; do not treat this note as a vulnerability waiver.

## Evidence and future decisions

The [implementation/evidence tracker](../docs/network-design-reference.md#implementation-and-evidence-tracker)
records named logical and physical checks. Three independent physical LANs, a
separate ordinary client host on the mini's LAN, native macOS WebView media and
physical camera/microphone quality remain distinct acceptance cases.

New protocol capabilities such as true multiplexing, automatic federation trust,
mailbox-backed calling, TURN or stronger replicated-storage guarantees require
an explicit design and new evidence. They are not implied by the implemented
capabilities above.
