# Mesh-Talk visual system — Quiet signal

Mesh-Talk is a desktop messenger for sustained use. The interface keeps the conversation as the primary reading plane. Navigation and composition form a stable frame around it. The visual language is calm and precise: warm mineral paper in light mode, charcoal green ink in dark mode, and one verdigris signal accent. The product never uses decoration to imply encryption or delivery state.

## Foundations

Tokens live in `src/index.css`; Tailwind maps them in `tailwind.config.js`. The standard light and dark palettes share the same geometry. OLED preserves a true-black canvas. Argentina, Barcelona, Messi, and Nature remain optional personal palettes with their own accents and wallpapers. In light branded themes, the wallpaper scrim is strong enough to keep messages and timestamps legible. Wallpaper can be disabled.

| Layer | Treatment | Role |
| --- | --- | --- |
| Navigation rail | `--shell-rail`, quiet directional light, hairline divider | Conversations, search, tools, account and connection |
| Conversation plane | `--conversation-surface`, restrained ambient light | Reading messages |
| Composer dock | `--composer-surface`, top divider and subtle upward separation | Stable writing area |
| Floating surface | `--popover`, `--card`, tinted elevation | Menus and dialogs only |
| Structure | `--border`, `--input` | Separators and input boundaries |
| Signal | `--signal`, `--primary` | Focus, selection, primary action |
| Status | `--presence-online`, `--presence-recent`, `--verified`, `--destructive` | Meaning always paired with text or an icon |
| Own message | `--bubble-own` | A distinct but comfortable message fill |

## Type, density and shape

The operating system UI font is the body font. Locally bundled Space Grotesk gives brand names, conversation titles and dialog headings a distinct voice without changing message readability. Monospace is reserved for fingerprints, safety numbers, IDs, ports and numeric progress. Message text is 14px with approximately 1.55 line height. Navigation labels are 13px; secondary text and timestamps are 11–12px with adequate contrast. Section labels are small, tracked capitals. Dialog titles and important empty-state headings are 16–18px. The sign-in product name is 24px.

Spacing follows a 4px unit. A conversation row has a 56px minimum height and a full-width click target. Outer surfaces use a 10px radius, with smaller controls nested inside. The selected row has a quiet fill and a 2px signal edge; color alone is not the sole locator because its position and selected state are also exposed semantically.

## Application frame

The sidebar defaults to 284px and remains user resizable. Search is directly below the app mark. Conversations are grouped into pinned, direct messages and channels. Row actions appear on hover and keyboard focus and remain available on touch. Arrow keys move through conversation options. Files, connection and settings are consistently placed above the account and connection summary.

The conversation header identifies the contact or channel, with history, membership, call and verification beside that identity. The message log and composer share an 820px maximum content width. Messages have distinct sent and received fills; delivery denotes account receipt, never a read receipt. Replies, mentions, pending or failed sends and file transfer retain distinct visible states.

The composer is a stable bottom dock. Attachment, media, expression and send controls remain near the input, and the tool row can collapse for a quieter writing area. Menus and dialogs use the same solid surfaces, hairline structure, typography and elevation. Settings retain their navigable section list; sign-in and identity creation use the same input and button system as the signed-in app.

## States and accessibility

Empty conversation and no-selection states have a quiet concentric motif, a clear explanation and the existing next action. Loading, connection, offline and error states say what is happening and offer retry or diagnostics when available. Technical values remain selectable and easy to compare. Text and controls must wrap or truncate within their pane, including long names, identifiers, messages and filenames.

Controls need accessible names, keyboard focus indicators and at least the existing touch target sizes. Native buttons handle Enter and Space; Escape closes Radix dialogs and menus. Brief 150–190ms motion is reserved for press feedback and floating surfaces. Ordinary message reading does not animate on entry. Reduced-motion settings remove spatial movement while preserving 80ms color and opacity feedback; progress that would otherwise be shown by a spinner has readable text. Theme previews use the actual canvas, signal and rail tokens. At narrow widths, the existing responsive conversation behavior remains intact; at short heights, dialogs scroll and tools remain reachable. Title-bar drag regions and native control insets remain clear on macOS, Windows and Linux.
