# Mesh-Talk visual system — Quiet signal

Mesh-Talk is a desktop messenger for sustained use. The interface keeps the conversation as the primary reading plane. Navigation and composition form a stable frame around it. The visual language is calm and precise: warm mineral paper in light mode, charcoal green ink in dark mode, and one verdigris signal accent. The product never uses decoration to imply encryption or delivery state.

## Foundations

Tokens live in `src/index.css`; Tailwind maps them in `tailwind.config.js`. The standard light and dark palettes share the same geometry. OLED preserves a true-black canvas. Argentina, Barcelona, Messi, and Nature remain optional personal palettes with their own accents and wallpapers. In light branded themes, the wallpaper scrim is strong enough to keep messages and timestamps legible. Wallpaper can be disabled.

| Layer              | Treatment                                                               | Role                                                 |
| ------------------ | ----------------------------------------------------------------------- | ---------------------------------------------------- |
| Navigation rail    | `--shell-rail`, quiet directional light, hairline divider               | Conversations, search, tools, account and connection |
| Conversation plane | `--conversation-surface`, restrained ambient light                      | Reading messages                                     |
| Composer dock      | `--composer-surface`, top divider and subtle upward separation          | Stable writing area                                  |
| Floating surface   | `--popover`, `--card`, tinted elevation                                 | Menus and dialogs only                               |
| Structure          | `--border`, `--input`                                                   | Separators and input boundaries                      |
| Signal             | `--signal`, `--primary`                                                 | Focus, selection, primary action                     |
| Status             | `--presence-online`, `--presence-recent`, `--verified`, `--destructive` | Meaning always paired with text or an icon           |
| Own message        | `--bubble-own`                                                          | A distinct but comfortable message fill              |

## Type, density and shape

The operating system UI font is the body font. Locally bundled Space Grotesk gives brand names, conversation titles and dialog headings a distinct voice without changing message readability. Monospace is reserved for fingerprints, safety numbers, IDs, ports and numeric progress. Message text is 14px with approximately 1.55 line height. Navigation labels are 13px; secondary text and timestamps are 11–12px with adequate contrast. Section labels are small, tracked capitals. Dialog titles and important empty-state headings are 16–18px. The sign-in product name is 24px.

Spacing follows a 4px unit. A conversation row has a 56px minimum height: its label opens the conversation, while a contact avatar opens that person's profile. Outer surfaces use a 10px radius, with smaller controls nested inside. The selected row has a quiet fill and a 2px signal edge; color alone is not the sole locator because its position and selected state are also exposed semantically.

## Application frame

The sidebar defaults to 284px and remains user resizable. Search is directly below the app mark. Conversations are grouped into pinned, direct messages and channels. Row actions appear on hover and keyboard focus and remain available on touch. Arrow keys move through conversation options. Received files occupies one utility row above the account summary. Connection and Settings sit in the footer menu alongside Link a device and About, keeping the conversation rail compact at every window height.

The conversation header identifies the contact or channel, with history, membership, call and verification beside that identity. Contact identities and channel authors open one member card showing full account and device IDs, presence, and known network endpoints. Missing endpoints are stated explicitly; a known endpoint can be a relay, not a physical location. A separate message action preserves the direct-message shortcut in member lists. The message log and composer share an 820px maximum content width. Messages have distinct sent and received fills; delivery denotes account receipt, never a read receipt. Replies, mentions, pending or failed sends and file transfer retain distinct visible states.

The composer is a stable bottom dock. Attachment, media, expression and send controls remain near the input, and the tool row can collapse for a quieter writing area. Menus and dialogs use the same solid surfaces, hairline structure, typography and elevation. Settings retain a navigable category list with one focused page at a time. Category pages reuse a shared acknowledged settings snapshot and accessible row/status patterns. Network disclosure and relay permissions are separate from local device privacy, and destructive retention reductions require confirmation; sign-in and identity creation use the same input and button system as the signed-in app.

## States and accessibility

Empty conversation and no-selection states have a quiet concentric motif, a clear explanation and the existing next action. Loading, connection, offline and error states say what is happening and offer retry or diagnostics when available. Technical values remain selectable and easy to compare. Text and controls must wrap or truncate within their pane, including long names, identifiers, messages and filenames.

People use one concise status line: Online, Online · slow connection, or Offline
with recent local observation history. Do not add a long quality paragraph.
Physical LAN observations expire at 15 seconds; cross-network observations keep
Online for 120 seconds from their last valid public observation. Fresh device
evidence remains independently bounded. A slow quality hint clears after thirty
healthy seconds and renewed public evidence, avoiding flicker. Withdrawal is
immediate. Returning observations use the existing five-second confirmation;
delivery progresses independently. The member profile shows fresh devices and
their overlapping discovery sources without turning tolerance into fresh evidence.
Group headers use accepted account totals and known-online people counts, with
unresolved device memberships stated separately. Source provenance does not imply
physical location or successful communication. A compact status disclosure in
the sidebar's connection footer announces unavailable online updates and retry.
It never overlays the conversation header. Expanding it explains potentially
stale indicators and reveals the latest refresh error; long errors wrap and
scroll within the footer. Successful refresh removes the notice. Evidence
continues to expire independently of this presentation.

Controls need accessible names, keyboard focus indicators and at least the existing touch target sizes. Native buttons handle Enter and Space; Escape closes Radix dialogs and menus. Brief 150–190ms motion is reserved for press feedback and floating surfaces. Ordinary message reading does not animate on entry. Reduced-motion settings remove spatial movement while preserving 80ms color and opacity feedback; progress that would otherwise be shown by a spinner has readable text. Theme previews use the actual canvas, signal and rail tokens. At narrow widths, the existing responsive conversation behavior remains intact; at short heights, dialogs scroll and tools remain reachable. Title-bar drag regions and native control insets remain clear on macOS, Windows and Linux.

Message receipt and copy signals sit beside the timestamp below each bubble.
Use small Lucide icons: a clock for awaiting receipt, a check for recipient
confirmation, a copy icon with 1 or 2+ for member copies, and an inbox for
post-office custody. An unknown copy observation uses a question mark beside
the copy icon; zero confirmed member copies uses the icon alone. Keep neutral
color and accessible names. Group copy evidence never uses delivery check marks.
Selecting a signal expands its explanation in the message flow; hover and focus
do not open overlays. Enter/Space toggles the disclosure, Escape collapses it
and restores focus. Reset disclosure on message, conversation or session changes.
Failure text, retry actions and file byte progress retain their separate meaning.


### Interaction motion

The shared JS curve is cubic-bezier(0.22, 1, 0.36, 1). Floating surfaces enter in 150–190ms and exit in 100–120ms through Radix presence. Pointer-selected tabs move a measured background in 170ms; keyboard selection updates immediately. Trigger resize observations keep the background aligned when labels change inside a fixed-size list. Only the absolute background interpolates its dimensions; neighboring controls retain their layout. Hidden or unmeasured tabs retain the ordinary selected background. Reaction press feedback affects only the emoji under a fine pointer, never synchronized counts or message layout.

Measured file progress interpolates transforms over 100ms with linear timing. Its presentation fades out over 120ms when the authoritative transfer becomes inactive or delivered; byte completion alone remains confirmation pending. Media retains its existing 220ms shared-element transition, with 80ms reduced-motion feedback. Welcome content enters once per mount in a bounded 280ms sequence, then stays still. Confirmed device linking may show a brief check entrance; errors never receive a success mark. No motion delays protocol actions, invents progress, or animates historical message entry.

Live-message entrance requires explicit live arrival evidence or a local pending intent with a client ID. Conversation, account and runtime scopes invalidate prior eligibility before rendering; history, search context and reading older messages suppress entrances. Eligibility survives deferred virtual row mounting and is consumed by the first committed row mount.

Message entrances observe the live system motion preference directly: enabling reduced motion stops active movement and settles messages immediately, including recalled messages. Successful account linking closes the pairing form and shows an eight-second dismissible confirmation above the viewport edge. This feedback survives node/runtime replacement but is scoped to the originating desktop owner and generation.

Pending composer images reserve a 160 × 80 frame before their local URL or decoded pixels are ready, keeping keyboard paste immediate and avoiding decode-driven layout shifts. Video poster caches retain original source dimensions atomically with the bounded JPEG, so the player starts with the same aspect ratio and natural-size ceiling before metadata arrives. Both sending and receiving progress use the same 100ms linear transform interpolation. Short desktop authentication uses the existing two-column layout through 740px of height so native credential guidance does not push the primary action below the first viewport.
