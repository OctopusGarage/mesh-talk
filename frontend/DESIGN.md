# Mesh-Talk visual system — Native direction

Mesh-Talk is a desktop messenger for sustained use. The interface should feel calm, direct, and technically trustworthy. Direction A in frontend/prototypes/mesh-talk is the visual reference: compact navigation, one clear conversation plane, restrained controls, and explicit status. Product language and behavior remain governed by the production app.

## Foundations

Tokens are in src/index.css; Tailwind mappings are in tailwind.config.js. Use semantic utilities and CSS variables. Dark, light, and OLED are base themes. The optional Argentina, Barcelona, and Messi palettes retain their identity; shared component geometry and hierarchy are the same in every theme.

| Layer            | Token / treatment                                               | Purpose                                                    |
| ---------------- | --------------------------------------------------------------- | ---------------------------------------------------------- |
| Rail             | --shell-rail                                                    | Conversation navigation, slightly distinct from the canvas |
| Canvas           | --conversation-surface                                          | Message reading area                                       |
| Composer         | --composer-surface                                              | Stable writing surface                                     |
| Floating surface | --popover, --card                                               | Menus and dialogs                                          |
| Structure        | --border, --input                                               | Hairline separation                                        |
| Action           | --signal, --primary                                             | Focus, selected actions, links                             |
| Status           | --presence-online, --presence-recent, --verified, --destructive | Distinct meanings that also have text or icons             |
| Own message      | --bubble-own                                                    | Quiet fill separate from the action accent                 |

Avoid decorative gradients and blur on ordinary UI surfaces. Existing optional themed conversation wallpapers are a user preference and can be turned off. Use shadows only on floating menus and dialogs. The normal layout is defined by flat surfaces and borders.

## Type and spacing

Use the operating system UI font for body and headings, and the system monospaced font for fingerprints, safety numbers, IDs, ports, and progress values. Do not use monospaced type merely to look technical. Name and section hierarchy comes from weight and spacing.

- Main body and messages: 13–14px; messages use 14px with approximately 1.55 line height.
- Navigation names and control labels: 13px. Secondary text and timestamps: 11–12px with sufficient contrast.
- Dialog titles and important empty-state headings: 16–18px. The sign-in product name may be 24px.
- Use a 4px spacing unit. Common gaps are 8, 12, 16, and 24px. A conversation row is compact but has a full-width click target.
- Radius is 8px at the large token, smaller for row controls. Message bubbles and dialogs use a restrained 8–12px radius.

## Component hierarchy

The sidebar is 284px by default and user resizable. It groups conversations, search, utilities, network status, and account access. Selected conversations use a quiet surface change. Unread count, presence, and time remain legible at rest; row actions appear on hover or keyboard focus and stay available on touch devices. Arrow keys move through conversation options.

The conversation header identifies the contact or channel and keeps history, membership, call, and verification actions adjacent to that identity. The message log and composer share an 820px maximum content width. Bubbles use different fills for sent and received messages; delivery means account receipt, never read receipt. Message metadata is quiet but readable. Replies, mentions, failed sends, pending sends, and file transfer have distinct visible states.

The composer is a stable bottom surface with a clear writing field and compact attachment, media, sticker, and send controls. Composer menus use the same menu radius, elevation, and typography as sidebar menus. Dialogs use solid surfaces, one border, a short title, and a direct primary action. Settings are organized as a navigable list of sections with compact rows and consistent controls. Onboarding uses the same controls and type scale as the signed-in app.

Empty, loading, error, connection, and offline states state what is happening and offer the existing next action where one exists. Do not rely on color alone for encryption, verification, or connection meaning. Keep fingerprints and safety numbers selectable and easy to compare. Technical diagnostics may use monospaced values, while explanatory text stays in the UI font.

## Interaction

All interactive controls need a visible keyboard focus state and an accessible name. Hover adds affordance without being required to discover core actions. Enter and Space activate native buttons; Escape closes Radix dialogs and menus. Respect reduced-motion preferences. Use brief transitions only for state changes, and avoid entrance animation in the ordinary message reading path.

At narrow widths, keep the existing responsive conversation behavior and make long names, identifiers, messages, and filenames wrap or truncate inside their panes. At short heights, tools remain accessible and dialog content scrolls. Native window drag regions and title-bar insets must remain clear on macOS, Windows, and Linux.
