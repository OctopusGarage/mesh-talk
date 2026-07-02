# UI/UX Audit and Medium Refresh Design

## Goal

Improve Mesh-Talk's visual and interaction quality across the high-frequency chat
experience without a full redesign. The work should make the app feel more consistent,
readable, and predictable while adding automated coverage for objective UI regressions.

## Scope

This is a medium refresh, not a system-level redesign. Keep the existing "Ink & Signal"
direction, theme architecture, component library, and core app structure. Prioritize the
daily chat path:

- Login-to-chat shell
- Sidebar identity, conversation list, pinned rows, channels, footer, and overflow menu
- Conversation header, empty state, message log, date separators, actions, reactions, and
  message lifecycle states
- Composer toolbar, text input, reply banner, emoji/sticker/screenshot/attachment flows
- High-frequency dialogs: Search, Files tray, History, Settings, Profile, Members, Verify
- Cross-cutting behavior: six themes, desktop and narrow widths, focus states, contrast,
  horizontal overflow, clipping, and modal viewport fit

Out of scope for this pass:

- New major product features
- Replacing the design language
- Reworking backend or protocol behavior
- Pixel-perfect screenshot baselines for every screen
- Subjective brand art decisions beyond clear usability issues

## Success Criteria

The refresh is successful when:

- The main chat flow is visibly more coherent and easier to scan.
- Sidebar, conversation, composer, and high-frequency dialogs share consistent spacing,
  hierarchy, button affordances, and status treatment.
- Objective UI regressions are caught automatically: unreadable text, overflowing bubbles,
  horizontal page overflow, clipped modals, hidden focus states, and too-small click targets.
- Existing E2E flows remain stable.
- Any visual/interaction change is justified by a specific audit finding or a documented
  consistency rule.

## Design Principles

### Keep The App Work-Focused

Mesh-Talk is a secure desktop messenger. The UI should stay dense enough for repeated use,
with restrained visual treatment and clear hierarchy. Avoid marketing-like hero layouts,
decorative cards inside cards, oversized headings in compact panels, and purely ornamental
effects.

### Prefer Systemic Fixes

When an issue appears in multiple places, fix the shared component or token instead of
patching individual screens. Examples: dialog content spacing, icon button sizing, message
link colors, popover menu item density, and focus-visible rings.

### Make Objective Quality Testable

The test suite should not try to prove the app is beautiful. It should prove the UI remains
usable:

- Text/background contrast is at least 4.5:1 for normal text.
- Important controls have visible focus styles.
- Icon buttons and row actions have adequate target dimensions.
- Message bubbles, composer, sidebars, and dialogs stay within their containers.
- Popovers/dialogs open, close, and fit the viewport.
- Theme changes do not break the above.

## Audit Matrix

### Shell And Sidebar

Inspect:

- Initial post-login layout at desktop and narrow widths
- Sidebar width and resize handle ergonomics
- Identity block density and clickable profile affordance
- Conversation row hierarchy: avatar, name, last/status text, favorite pin, active state
- Pinned/DM/channel section balance
- Footer network/status treatment and overflow menu discoverability
- Empty/stranded states

Expected improvements:

- Rows should be easy to scan without text crowding icons.
- Active, hover, focus, unread, pinned, and offline states should be visually distinct.
- Footer controls should not collide with theme crest, network text, or resize handle.

Automated coverage:

- No horizontal shell overflow at desktop and narrow widths.
- Conversation row content stays inside row bounds.
- Sidebar action buttons meet minimum target size and have accessible names.
- Focus-visible state appears on sidebar row/action keyboard navigation.

### Conversation Header And Empty State

Inspect:

- Contact/channel identity presentation
- Header action grouping: history, members, verify, call controls
- Long names, aliases, member counts, and narrow widths
- Empty state with base themes and brand palettes

Expected improvements:

- Header should identify the conversation quickly without crowding actions.
- Secondary metadata should be subdued but readable.
- Empty state should guide without becoming a landing page.

Automated coverage:

- Header controls stay within header bounds.
- Long names truncate cleanly.
- Header buttons meet target size and focus criteria.

### Message Log

Inspect:

- Incoming/outgoing bubble contrast and spacing
- Link, mention, reaction, pending, failed, recalled, reply-snippet, file, media, and sticker states
- Message action reveal on hover and keyboard/context menu usability
- Date separators and channel author labels
- Very long messages, URLs, CJK text, emoji, and multi-line code-like text

Expected improvements:

- Message content should remain readable in all six themes.
- Actions should be discoverable without making the log visually noisy.
- Failure/retry and recalled states should be clear but not dominate.

Automated coverage:

- Contrast checks for sent/received text, links, mentions, and failure text.
- Bubble bounds checks for long unbreakable content and narrow widths.
- Context menu actions open within viewport and remain keyboard reachable.
- Reaction chips do not overflow the log.

### Composer

Inspect:

- Toolbar placement, icon button affordance, input height, send button state
- Reply banner, mention autocomplete, emoji/sticker popovers, screenshot menu
- Attachment/image button distinction
- Narrow-width wrapping and focus flow

Expected improvements:

- The composer should make primary typing and sending feel direct.
- Secondary tools should be available without stealing vertical space.
- Popovers should close predictably and not obscure the input unnecessarily.

Automated coverage:

- Composer controls remain above/beside input as designed at desktop and narrow widths.
- Toolbar icon buttons have accessible names, minimum target size, and visible focus.
- Popovers fit the viewport and close on outside click/Escape.
- Reply banner does not push composer outside viewport.

### High-Frequency Dialogs

Inspect:

- Search dialog
- Files tray
- Conversation history
- Settings and theme picker
- Profile dialog and avatar crop/gallery
- Members dialog
- Verify dialog

Expected improvements:

- Dialogs should use consistent header/body/footer rhythm.
- Lists should have clear empty states.
- Dense technical content should use mono only where comparison matters.
- Dialog content should scroll internally when needed instead of overflowing the viewport.

Automated coverage:

- Each high-frequency dialog opens and fits in `1280x800` and a narrow viewport.
- Dialogs have accessible titles and close controls.
- Primary actions are focusable and not obscured.
- No internal content spills outside dialog bounds.

## Testing Architecture

### Unit/Token Tests

Keep and expand CSS/token-level tests where objective values can be computed cheaply.
Examples:

- Theme token contrast tests for message text and links
- Pure text segmentation/link rendering tests
- Utility tests for history categorization, media type decisions, and formatting

### Browser UI Audit Helpers

Add a small Playwright helper module under `frontend/e2e/helpers/ui-audit.ts` that exposes:

- `expectNoHorizontalOverflow(page, scope)`
- `expectElementsWithin(page, childSelector, parentSelector)`
- `expectMinTargetSize(locator, minPx = 32)`
- `expectVisibleFocus(page, locator)`
- `contrastRatio(foreground, background)` used inside `page.evaluate`
- `expectDialogFitsViewport(page, dialogTestId)`

Helpers should return useful failure messages with element test IDs and measured values.

### Scenario E2E Tests

Add focused E2E specs instead of one giant fragile test:

- `ui-audit-shell.spec.ts`: shell, sidebar, footer, rows, resize/narrow checks
- `ui-audit-message-log.spec.ts`: message states, contrast, overflow, action menus
- `ui-audit-composer.spec.ts`: composer controls, popovers, reply banner, focus
- `ui-audit-dialogs.spec.ts`: search, files, history, settings, profile, members, verify

Existing E2E specs should remain as behavior regression tests. New audit specs should
measure layout and accessibility invariants.

### Screenshot Coverage

Use screenshot snapshots sparingly. Prefer numeric assertions for layout and contrast.
Add screenshots only for stable, high-value states:

- Main chat shell in dark theme
- Main chat shell in a brand theme
- Narrow-width conversation with composer

Do not add broad full-page snapshots for every dialog; they are noisy and expensive.

## Implementation Strategy

1. Build the UI audit helper layer first.
2. Add failing audit coverage for the most important current gaps.
3. Fix shared primitives/tokens before individual screens.
4. Refresh main chat surfaces in this order:
   - Sidebar and shell
   - Conversation header and message log
   - Composer
   - High-frequency dialogs
5. Run focused E2E after each surface, then full frontend validation before completion.

## Risks And Mitigations

- **Risk:** Audit tests become brittle.
  **Mitigation:** Prefer numeric invariants and semantic locators over screenshots and
  exact pixels.

- **Risk:** Medium refresh drifts into full redesign.
  **Mitigation:** Each change must map to an audit finding or consistency rule.

- **Risk:** E2E runtime grows too much.
  **Mitigation:** Group audit checks by already-opened app state and avoid repeated login
  setup where possible.

- **Risk:** Visual improvements break existing workflows.
  **Mitigation:** Keep existing behavior E2E specs and run full `npm run e2e` before final
  completion.

## Validation Commands

Minimum validation for the implementation:

```bash
cd frontend
npm run format:check
npm run test
npm run typecheck
npm run lint
npm run build
npm run e2e
```

For focused iteration:

```bash
cd frontend
npx playwright test e2e/ui-audit-shell.spec.ts --project=chromium
npx playwright test e2e/ui-audit-message-log.spec.ts --project=chromium
npx playwright test e2e/ui-audit-composer.spec.ts --project=chromium
npx playwright test e2e/ui-audit-dialogs.spec.ts --project=chromium
```
