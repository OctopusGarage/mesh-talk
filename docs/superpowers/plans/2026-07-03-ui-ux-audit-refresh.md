# UI/UX Audit Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build objective UI audit coverage for Mesh-Talk's main chat experience, then apply a medium visual/interaction refresh to the sidebar, message log, composer, and high-frequency dialogs.

**Architecture:** Add shared Playwright audit helpers first, then layer focused audit specs over the existing mocked Tauri E2E harness. Fix shared primitives/tokens before individual screens so visual quality improves systemically rather than through one-off patches.

**Tech Stack:** React 18, TypeScript, Tailwind, shadcn/Radix primitives, Tauri API mock, Vitest, Playwright Chromium E2E.

---

## File Structure

- Create `frontend/e2e/helpers/session.ts`: shared login/open-conversation helpers for E2E specs.
- Create `frontend/e2e/helpers/ui-audit.ts`: shared Playwright assertions for overflow, bounds, target size, focus, contrast, and dialog viewport fit.
- Create `frontend/e2e/ui-audit-shell.spec.ts`: shell/sidebar layout, target-size, and focus audit.
- Create `frontend/e2e/ui-audit-message-log.spec.ts`: message state layout/contrast/menu audit.
- Create `frontend/e2e/ui-audit-composer.spec.ts`: composer toolbar/input/popover/focus audit.
- Create `frontend/e2e/ui-audit-dialogs.spec.ts`: high-frequency dialog viewport/accessibility audit.
- Modify `frontend/src/features/chat/Sidebar.tsx`: row action target sizing, hover/focus discoverability, footer small controls.
- Modify `frontend/src/components/ui/dialog.tsx`: viewport-safe dialog content and close button target.
- Modify `frontend/src/features/chat/MessageBubble.tsx`: message action target sizing and menu/failure state polish if audit exposes failures.
- Modify `frontend/src/features/chat/Composer.tsx`: popover viewport fit and focus/target refinements if audit exposes failures.
- Modify individual dialog files only when shared `DialogContent` does not solve the measured viewport issue.

## Task 1: Add Shared UI Audit Helpers

**Files:**
- Create: `frontend/e2e/helpers/session.ts`
- Create: `frontend/e2e/helpers/ui-audit.ts`
- Create: `frontend/e2e/ui-audit-shell.spec.ts`

- [ ] **Step 1: Write the failing shell audit spec**

Create `frontend/e2e/ui-audit-shell.spec.ts`:

```ts
import { test, expect } from "./tauri-mock";
import { enterChat, BOB } from "./helpers/session";
import {
  expectElementsWithin,
  expectMinTargetSize,
  expectNoHorizontalOverflow,
  expectVisibleFocus,
} from "./helpers/ui-audit";

test.use({ viewport: { width: 1100, height: 760 } });

test("shell and sidebar meet baseline layout and interaction invariants", async ({
  page,
}) => {
  await enterChat(page);

  await expectNoHorizontalOverflow(page, "chat shell");
  await expectElementsWithin(
    page,
    '[data-testid^="conversation-row-"]',
    '[data-testid="sidebar"]',
  );

  await page.getByTestId(`conversation-row-${BOB.account}`).hover();
  for (const id of [
    "open-profile",
    "sidebar-overflow",
    `conversation-pin-${BOB.account}`,
  ]) {
    await expectMinTargetSize(page.getByTestId(id), 32);
  }

  await expectVisibleFocus(page, page.getByTestId("sidebar-overflow"));
  await expectVisibleFocus(page, page.getByTestId(`conversation-row-${BOB.account}`));
});
```

- [ ] **Step 2: Run the spec to verify it fails because helpers do not exist**

Run:

```bash
cd frontend
npx playwright test e2e/ui-audit-shell.spec.ts --project=chromium
```

Expected: FAIL with a module resolution error for `./helpers/session` or
`./helpers/ui-audit`.

- [ ] **Step 3: Add the shared session helper**

Create `frontend/e2e/helpers/session.ts`:

```ts
import type { Page } from "@playwright/test";
import { expect } from "../tauri-mock";

export const BOB = { account: "acc_bob_bbbb2222", device: "device_bob_2222" };
export const CAROL = {
  account: "acc_carol_cccc3333",
  device: "device_carol_3333",
};
export const CHANNEL = { id: "chan_team_dddd4444" };

export async function enterChat(page: Page, user = "tester") {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill(user);
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await expect(page.getByTestId(`conversation-row-${BOB.account}`)).toBeVisible();
}

export async function openBobDm(page: Page) {
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await expect(page.getByText("hey, welcome to the mesh")).toBeVisible();
}
```

- [ ] **Step 4: Add the shared UI audit helper**

Create `frontend/e2e/helpers/ui-audit.ts`:

```ts
import type { Locator, Page } from "@playwright/test";
import { expect } from "../tauri-mock";

export async function expectNoHorizontalOverflow(page: Page, label: string) {
  const metrics = await page.evaluate(() => ({
    documentScrollWidth: document.documentElement.scrollWidth,
    documentClientWidth: document.documentElement.clientWidth,
    bodyScrollWidth: document.body.scrollWidth,
    bodyClientWidth: document.body.clientWidth,
  }));
  expect(
    metrics.documentScrollWidth,
    `${label}: document overflows horizontally`,
  ).toBeLessThanOrEqual(metrics.documentClientWidth + 1);
  expect(
    metrics.bodyScrollWidth,
    `${label}: body overflows horizontally`,
  ).toBeLessThanOrEqual(metrics.bodyClientWidth + 1);
}

export async function expectElementsWithin(
  page: Page,
  childSelector: string,
  parentSelector: string,
) {
  const failures = await page.evaluate(
    ({ childSelector, parentSelector }) => {
      const parent = document.querySelector(parentSelector) as HTMLElement | null;
      if (!parent) return [`missing parent ${parentSelector}`];
      const pr = parent.getBoundingClientRect();
      return Array.from(document.querySelectorAll<HTMLElement>(childSelector))
        .map((child) => {
          const cr = child.getBoundingClientRect();
          const id =
            child.getAttribute("data-testid") ||
            child.getAttribute("aria-label") ||
            child.tagName;
          const outside =
            cr.left < pr.left - 1 ||
            cr.right > pr.right + 1 ||
            cr.top < pr.top - 1 ||
            cr.bottom > pr.bottom + 1;
          return outside
            ? `${id} outside ${parentSelector}: child=${JSON.stringify({
                left: cr.left,
                right: cr.right,
                top: cr.top,
                bottom: cr.bottom,
              })} parent=${JSON.stringify({
                left: pr.left,
                right: pr.right,
                top: pr.top,
                bottom: pr.bottom,
              })}`
            : null;
        })
        .filter(Boolean);
    },
    { childSelector, parentSelector },
  );
  expect(failures).toEqual([]);
}

export async function expectMinTargetSize(locator: Locator, minPx = 32) {
  await expect(locator).toBeVisible();
  const box = await locator.boundingBox();
  expect(box, "target has no bounding box").not.toBeNull();
  expect(box!.width, "target width too small").toBeGreaterThanOrEqual(minPx);
  expect(box!.height, "target height too small").toBeGreaterThanOrEqual(minPx);
}

export async function expectVisibleFocus(page: Page, locator: Locator) {
  await locator.focus();
  const focus = await locator.evaluate((el) => {
    const style = getComputedStyle(el);
    return {
      outlineStyle: style.outlineStyle,
      outlineWidth: style.outlineWidth,
      boxShadow: style.boxShadow,
      active: el.matches(":focus-visible") || el === document.activeElement,
    };
  });
  expect(focus.active).toBe(true);
  expect(
    focus.boxShadow !== "none" ||
      (focus.outlineStyle !== "none" && focus.outlineWidth !== "0px"),
    "focused element has no visible focus treatment",
  ).toBe(true);
  await page.keyboard.press("Escape");
}

export function contrastRatio(foreground: string, background: string) {
  const parseRgb = (value: string) => {
    const channels = value.match(/[\d.]+/g)?.map(Number);
    if (!channels || channels.length < 3) {
      throw new Error(`Cannot parse color: ${value}`);
    }
    return channels.slice(0, 3).map((channel) => channel / 255);
  };
  const luminance = (value: string) => {
    const [red, green, blue] = parseRgb(value).map((channel) =>
      channel <= 0.03928
        ? channel / 12.92
        : ((channel + 0.055) / 1.055) ** 2.4,
    );
    return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
  };
  const fg = luminance(foreground);
  const bg = luminance(background);
  return (Math.max(fg, bg) + 0.05) / (Math.min(fg, bg) + 0.05);
}

export async function expectDialogFitsViewport(page: Page, dialogTestId: string) {
  const result = await page.getByTestId(dialogTestId).evaluate((dialog) => {
    const rect = dialog.getBoundingClientRect();
    return {
      left: rect.left,
      right: rect.right,
      top: rect.top,
      bottom: rect.bottom,
      width: window.innerWidth,
      height: window.innerHeight,
    };
  });
  expect(result.left, `${dialogTestId} clips left`).toBeGreaterThanOrEqual(8);
  expect(result.top, `${dialogTestId} clips top`).toBeGreaterThanOrEqual(8);
  expect(result.right, `${dialogTestId} clips right`).toBeLessThanOrEqual(
    result.width - 8,
  );
  expect(result.bottom, `${dialogTestId} clips bottom`).toBeLessThanOrEqual(
    result.height - 8,
  );
}
```

- [ ] **Step 5: Run the shell audit spec again**

Run:

```bash
cd frontend
npx playwright test e2e/ui-audit-shell.spec.ts --project=chromium
```

Expected: FAIL on `conversation-pin-acc_bob_bbbb2222` target height/width being below
32px. This proves the helper finds a real shell/sidebar interaction issue.

- [ ] **Step 6: Commit the helper layer and failing shell audit**

```bash
git add frontend/e2e/helpers/session.ts frontend/e2e/helpers/ui-audit.ts frontend/e2e/ui-audit-shell.spec.ts
git commit -m "test(ui): add shared ui audit helpers"
```

## Task 2: Refresh Sidebar Interaction Targets

**Files:**
- Modify: `frontend/src/features/chat/Sidebar.tsx`
- Test: `frontend/e2e/ui-audit-shell.spec.ts`

- [ ] **Step 1: Keep the failing shell audit as the red test**

Run:

```bash
cd frontend
npx playwright test e2e/ui-audit-shell.spec.ts --project=chromium
```

Expected: FAIL on row action target size for the pin action.

- [ ] **Step 2: Update row action buttons to have stable 32px targets**

In `frontend/src/features/chat/Sidebar.tsx`, replace the two row action button class blocks
inside `Row` with stable, focusable targets:

```tsx
      <button
        type="button"
        onClick={onRename}
        title={t("sidebar.rename")}
        aria-label={t("sidebar.rename")}
        className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-muted-foreground opacity-0 transition-[background-color,color,opacity] hover:bg-accent hover:text-foreground focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring group-hover:opacity-100"
      >
        <Pencil className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={onTogglePin}
        data-testid={`conversation-pin-${conv.id}`}
        title={pinned ? t("sidebar.unpin") : t("sidebar.pin")}
        aria-label={pinned ? t("sidebar.unpin") : t("sidebar.pin")}
        className={cn(
          "flex h-8 w-8 shrink-0 items-center justify-center rounded-md transition-[background-color,color,opacity] hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
          pinned
            ? "text-signal opacity-100"
            : "text-muted-foreground opacity-0 hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100",
        )}
      >
        {pinned ? (
          <PinOff className="h-3.5 w-3.5" />
        ) : (
          <Pin className="h-3.5 w-3.5" />
        )}
      </button>
```

- [ ] **Step 3: Update stranded prompt dismiss target**

In `frontend/src/features/chat/Sidebar.tsx`, replace the stranded dismiss button class
with a 32px target:

```tsx
className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
```

- [ ] **Step 4: Run the shell audit**

Run:

```bash
cd frontend
npx playwright test e2e/ui-audit-shell.spec.ts --project=chromium
```

Expected: PASS.

- [ ] **Step 5: Run related existing E2E specs**

Run:

```bash
cd frontend
npx playwright test e2e/theme-picker.spec.ts e2e/lan-count.spec.ts e2e/network-name.spec.ts --project=chromium
```

Expected: PASS.

- [ ] **Step 6: Commit sidebar refresh**

```bash
git add frontend/src/features/chat/Sidebar.tsx frontend/e2e/ui-audit-shell.spec.ts
git commit -m "fix(ui): improve sidebar action targets"
```

## Task 3: Add Dialog Viewport Audit And Shared Dialog Fit

**Files:**
- Create: `frontend/e2e/ui-audit-dialogs.spec.ts`
- Modify: `frontend/src/components/ui/dialog.tsx`

- [ ] **Step 1: Write the failing dialog audit**

Create `frontend/e2e/ui-audit-dialogs.spec.ts`:

```ts
import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
import { expectDialogFitsViewport, expectMinTargetSize } from "./helpers/ui-audit";

test.use({ viewport: { width: 820, height: 620 } });

test("high-frequency dialogs fit the viewport and expose usable close targets", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);

  const cases = [
    {
      open: async () => page.getByTestId("sidebar-action-search").click(),
      dialog: "search-dialog",
    },
    {
      open: async () => page.getByTestId("sidebar-action-files").click(),
      dialog: "files-tray",
    },
    {
      open: async () => page.getByTestId("conversation-history-trigger").click(),
      dialog: "conversation-history-dialog",
    },
    {
      open: async () => {
        await page.getByTestId("sidebar-overflow").click();
        await page.getByTestId("sidebar-action-settings").click();
      },
      dialog: "settings-dialog",
    },
    {
      open: async () => page.getByTestId("open-profile").click(),
      dialog: "profile-dialog",
    },
  ];

  for (const item of cases) {
    await item.open();
    await expect(page.getByTestId(item.dialog)).toBeVisible();
    await expectDialogFitsViewport(page, item.dialog);
    await expectMinTargetSize(
      page.getByTestId(item.dialog).getByRole("button", { name: "Close" }),
      32,
    );
    await page.keyboard.press("Escape");
    await expect(page.getByTestId(item.dialog)).toHaveCount(0);
  }
});
```

- [ ] **Step 2: Run the dialog audit to verify current failure**

Run:

```bash
cd frontend
npx playwright test e2e/ui-audit-dialogs.spec.ts --project=chromium
```

Expected: FAIL if any high-frequency dialog clips at `820x620` or if the Radix close
button target is below 32px.

- [ ] **Step 3: Make the shared dialog primitive viewport-safe**

In `frontend/src/components/ui/dialog.tsx`, replace the `DialogContent` base class with:

```tsx
"fixed left-1/2 top-1/2 z-50 grid max-h-[calc(100vh-2rem)] w-[calc(100vw-2rem)] max-w-md -translate-x-1/2 -translate-y-1/2 gap-4 overflow-y-auto rounded-2xl border bg-card p-6 shadow-elevation-lg duration-200 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95"
```

Replace the close button class with:

```tsx
"absolute right-3 top-3 flex h-8 w-8 items-center justify-center rounded-md opacity-70 ring-offset-background transition-opacity hover:opacity-100 focus:outline-none focus:ring-2 focus:ring-ring"
```

- [ ] **Step 4: Run dialog audit**

Run:

```bash
cd frontend
npx playwright test e2e/ui-audit-dialogs.spec.ts --project=chromium
```

Expected: PASS.

- [ ] **Step 5: Run existing dialog-heavy E2E specs**

Run:

```bash
cd frontend
npx playwright test e2e/chat.spec.ts e2e/message-lifecycle.spec.ts e2e/avatar-gallery.spec.ts e2e/avatar-crop.spec.ts --project=chromium
```

Expected: PASS.

- [ ] **Step 6: Commit dialog fit refresh**

```bash
git add frontend/e2e/ui-audit-dialogs.spec.ts frontend/src/components/ui/dialog.tsx
git commit -m "fix(ui): keep dialogs within viewport"
```

## Task 4: Add Message Log Audit And Refresh Message Actions

**Files:**
- Create: `frontend/e2e/ui-audit-message-log.spec.ts`
- Modify: `frontend/src/features/chat/MessageBubble.tsx`

- [ ] **Step 1: Write the failing message-log audit**

Create `frontend/e2e/ui-audit-message-log.spec.ts`:

```ts
import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
import { expectMinTargetSize, expectNoHorizontalOverflow } from "./helpers/ui-audit";

test.use({ viewport: { width: 1100, height: 760 } });

test("message log actions, menus, and reaction chips stay usable", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);

  const firstBubble = page.getByTestId("message-bubble").first();
  await firstBubble.hover();
  await expectMinTargetSize(firstBubble.getByTestId("message-reply"), 32);
  await expectMinTargetSize(firstBubble.getByTestId("message-react"), 32);

  await firstBubble.getByTestId("message-react").click();
  await expect(page.getByTestId("reaction-picker")).toBeVisible();
  await page.getByTestId("reaction-option-🔥").click();
  await expect(page.getByTestId("reaction-chip").first()).toBeVisible();
  await expectNoHorizontalOverflow(page, "message log after reaction");

  await firstBubble.getByText("hey, welcome to the mesh").click({ button: "right" });
  await expect(page.getByTestId("message-context-menu")).toBeVisible();
  const menuBox = await page.getByTestId("message-context-menu").boundingBox();
  expect(menuBox).not.toBeNull();
  expect(menuBox!.x).toBeGreaterThanOrEqual(0);
  expect(menuBox!.y).toBeGreaterThanOrEqual(0);
});
```

- [ ] **Step 2: Run the message-log audit to verify current failure**

Run:

```bash
cd frontend
npx playwright test e2e/ui-audit-message-log.spec.ts --project=chromium
```

Expected: FAIL on message hover action targets below 32px.

- [ ] **Step 3: Increase message hover action targets**

In `frontend/src/features/chat/MessageBubble.tsx`, update the two buttons inside
`Actions` from `p-1` to stable 32px target classes:

```tsx
className="flex h-8 w-8 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-30"
```

Apply that exact class to both `message-reply` and `message-react`.

- [ ] **Step 4: Make reaction chips keyboard-focus visible**

In `frontend/src/features/chat/MessageBubble.tsx`, add focus-visible ring classes to the
reaction chip button class list:

```tsx
"flex items-center gap-1 rounded-full border px-1.5 py-0.5 text-xs transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
```

- [ ] **Step 5: Run message-log audit and existing message specs**

Run:

```bash
cd frontend
npx playwright test e2e/ui-audit-message-log.spec.ts e2e/wide-message.spec.ts e2e/narrow-width.spec.ts e2e/message-lifecycle.spec.ts --project=chromium
```

Expected: PASS.

- [ ] **Step 6: Commit message-log refresh**

```bash
git add frontend/e2e/ui-audit-message-log.spec.ts frontend/src/features/chat/MessageBubble.tsx
git commit -m "fix(ui): improve message action affordances"
```

## Task 5: Add Composer Audit And Refresh Popover Fit

**Files:**
- Create: `frontend/e2e/ui-audit-composer.spec.ts`
- Modify: `frontend/src/features/chat/Composer.tsx`

- [ ] **Step 1: Write the composer audit**

Create `frontend/e2e/ui-audit-composer.spec.ts`:

```ts
import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
import {
  expectElementsWithin,
  expectMinTargetSize,
  expectNoHorizontalOverflow,
  expectVisibleFocus,
} from "./helpers/ui-audit";

test.use({ viewport: { width: 820, height: 620 } });

test("composer controls and popovers stay usable at narrow width", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);

  for (const id of [
    "composer-screenshot",
    "composer-attach",
    "composer-image",
    "composer-emoji",
    "composer-stickers",
    "composer-send",
  ]) {
    await expectMinTargetSize(page.getByTestId(id), 36);
    await expectVisibleFocus(page, page.getByTestId(id));
  }

  await page.getByTestId("composer-emoji").click();
  await expect(page.getByTestId("emoji-picker")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="emoji-picker"]', "body");
  await page.keyboard.press("Escape");

  await page.getByTestId("composer-stickers").click();
  await expect(page.getByTestId("sticker-panel")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="sticker-panel"]', "body");
  await page.keyboard.press("Escape");

  await page.getByTestId("composer-screenshot").click();
  await expect(page.getByTestId("screenshot-menu")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="screenshot-menu"]', "body");
  await expectNoHorizontalOverflow(page, "composer popovers");
});
```

- [ ] **Step 2: Run composer audit**

Run:

```bash
cd frontend
npx playwright test e2e/ui-audit-composer.spec.ts --project=chromium
```

Expected: FAIL if a popover clips outside the viewport or a composer action has no visible
focus treatment.

- [ ] **Step 3: Make composer popovers viewport-aware**

In `frontend/src/features/chat/Composer.tsx`, update popover wrappers:

For `emoji-picker`, use:

```tsx
className="absolute bottom-full left-0 mb-2 max-h-72 w-64 max-w-[calc(100vw-2rem)] overflow-y-auto rounded-xl border bg-popover p-2 shadow-elevation"
```

For `sticker-panel`, use:

```tsx
className="absolute bottom-full left-0 mb-2 max-h-72 w-80 max-w-[calc(100vw-2rem)] overflow-y-auto rounded-xl border bg-popover p-2 shadow-elevation"
```

For `screenshot-menu`, use:

```tsx
className="absolute bottom-full left-0 mb-2 w-56 max-w-[calc(100vw-2rem)] overflow-hidden rounded-xl border bg-popover p-1 shadow-elevation"
```

- [ ] **Step 4: Add focus-visible styles to sticker and screenshot menu choices**

In `frontend/src/features/chat/Composer.tsx`, add focus-visible ring classes to:

```tsx
className="rounded-lg p-1 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
```

and screenshot menu buttons:

```tsx
className="block w-full rounded-lg px-3 py-2 text-left text-sm hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
```

- [ ] **Step 5: Run composer audit and related specs**

Run:

```bash
cd frontend
npx playwright test e2e/ui-audit-composer.spec.ts e2e/composer-layout.spec.ts e2e/stickers.spec.ts --project=chromium
```

Expected: PASS.

- [ ] **Step 6: Commit composer refresh**

```bash
git add frontend/e2e/ui-audit-composer.spec.ts frontend/src/features/chat/Composer.tsx
git commit -m "fix(ui): improve composer popover usability"
```

## Task 6: Consolidate Theme Contrast Audit Helper Usage

**Files:**
- Modify: `frontend/e2e/theme-contrast.spec.ts`
- Modify: `frontend/e2e/helpers/ui-audit.ts`

- [ ] **Step 1: Refactor theme contrast spec to use the shared contrast helper**

In `frontend/e2e/theme-contrast.spec.ts`, remove the duplicated inline `contrast`
implementation and import `contrastRatio`:

```ts
import { contrastRatio } from "./helpers/ui-audit";
```

Inside `page.evaluate`, keep only DOM querying and return computed foreground/background
strings. Compute ratios in Node-side test code:

```ts
const checks = await page.evaluate(/* returns colors per theme */);
for (const check of checks) {
  expect(
    contrastRatio(check.sentTextColor, check.sentBubbleBackground),
    `${check.theme} sent text`,
  ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
  expect(
    contrastRatio(check.sentLinkColor, check.sentBubbleBackground),
    `${check.theme} sent link`,
  ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
  expect(
    contrastRatio(check.receivedLinkColor, check.receivedBubbleBackground),
    `${check.theme} received link`,
  ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
  expect(check.bodyOverflows, `${check.theme} body overflow`).toBe(false);
  expect(check.bubbleOverflows, `${check.theme} bubble overflow`).toBe(false);
}
```

- [ ] **Step 2: Run the theme contrast spec**

Run:

```bash
cd frontend
npx playwright test e2e/theme-contrast.spec.ts --project=chromium
```

Expected: PASS with the same coverage as before, now using the shared helper.

- [ ] **Step 3: Commit helper consolidation**

```bash
git add frontend/e2e/theme-contrast.spec.ts frontend/e2e/helpers/ui-audit.ts
git commit -m "refactor(test): share ui contrast audit helper"
```

## Task 7: Final Full Validation

**Files:**
- No source files expected beyond previous tasks.

- [ ] **Step 1: Run frontend formatting**

Run:

```bash
cd frontend
npm run format:check
npx prettier --check e2e/**/*.ts
```

Expected: PASS.

- [ ] **Step 2: Run unit tests, typecheck, lint, and build**

Run:

```bash
cd frontend
npm run test
npm run typecheck
npm run lint
npm run build
```

Expected: PASS:

- Vitest reports all test files passed.
- TypeScript exits 0.
- ESLint exits 0.
- Vite build exits 0.

- [ ] **Step 3: Run focused UI audit specs**

Run:

```bash
cd frontend
npx playwright test \
  e2e/ui-audit-shell.spec.ts \
  e2e/ui-audit-message-log.spec.ts \
  e2e/ui-audit-composer.spec.ts \
  e2e/ui-audit-dialogs.spec.ts \
  e2e/theme-contrast.spec.ts \
  --project=chromium
```

Expected: PASS.

- [ ] **Step 4: Run full frontend E2E**

Run:

```bash
cd frontend
npm run e2e
```

Expected: PASS for the full Playwright suite.

- [ ] **Step 5: Commit final validation note if any docs changed**

If no files changed after validation, do not commit. If validation caused intentional doc
updates, commit only those docs:

```bash
git status --short
git add <changed-doc-files>
git commit -m "docs: record ui audit validation"
```

Expected: working tree clean after the final implementation commits.

## Self-Review

- Spec coverage: The plan covers shell/sidebar, conversation/message log, composer,
  high-frequency dialogs, six-theme contrast, narrow width, focus, target size, overflow,
  and full validation.
- Placeholder scan: No task uses placeholder language; each task names exact files,
  commands, expected failures, and concrete code changes.
- Type consistency: Helper names are consistent across specs:
  `expectNoHorizontalOverflow`, `expectElementsWithin`, `expectMinTargetSize`,
  `expectVisibleFocus`, `contrastRatio`, and `expectDialogFitsViewport`.
