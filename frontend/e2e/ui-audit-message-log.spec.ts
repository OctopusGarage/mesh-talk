import type { Locator } from "@playwright/test";
import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
import {
  expectMinTargetSize,
  expectNoHorizontalOverflow,
  expectVisibleFocus,
} from "./helpers/ui-audit";

test.use({ viewport: { width: 1100, height: 760 } });

test("message log actions, menus, and reaction chips stay usable", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);

  const firstBubble = page.getByTestId("message-bubble").first();
  const replyAction = firstBubble.getByTestId("message-reply");
  const reactAction = firstBubble.getByTestId("message-react");
  await firstBubble.hover();
  await expectMinTargetSize(replyAction, 32);
  await expectMinTargetSize(reactAction, 32);

  await page.mouse.move(0, 0);
  await expectVisibleFocus(page, replyAction);
  await replyAction.focus();
  await expectActionRowVisible(replyAction);
  await expectVisibleFocus(page, reactAction);
  await reactAction.focus();
  await expectActionRowVisible(reactAction);

  await reactAction.click();
  await expect(page.getByTestId("reaction-picker")).toBeVisible();
  await page.getByTestId("reaction-option-🔥").click();
  const firstReactionChip = page.getByTestId("reaction-chip").first();
  await expect(firstReactionChip).toBeVisible();
  await expectVisibleFocus(page, firstReactionChip);
  await expectNoHorizontalOverflow(page, "message log after reaction");

  await page.getByTestId("composer-input").fill("bottom edge menu target");
  await page.getByTestId("composer-send").click();
  const edgeBubble = page.getByTestId("message-bubble").last();
  const messageText = edgeBubble.getByText("bottom edge menu target");
  await messageText.scrollIntoViewIfNeeded();
  const textBox = await messageText.boundingBox();
  expect(textBox).not.toBeNull();
  await messageText.click({
    button: "right",
    position: {
      x: Math.max(1, textBox!.width - 1),
      y: Math.max(1, textBox!.height - 1),
    },
  });
  await expect(page.getByTestId("message-context-menu")).toBeVisible();
  const menuBox = await page.getByTestId("message-context-menu").boundingBox();
  expect(menuBox).not.toBeNull();
  const viewport = page.viewportSize();
  expect(viewport).not.toBeNull();
  expect(menuBox!.x).toBeGreaterThanOrEqual(0);
  expect(menuBox!.y).toBeGreaterThanOrEqual(0);
  expect(menuBox!.x + menuBox!.width).toBeLessThanOrEqual(viewport!.width);
  expect(menuBox!.y + menuBox!.height).toBeLessThanOrEqual(viewport!.height);
});

async function expectActionRowVisible(action: Locator) {
  await expect(action).toBeFocused();
  await expect
    .poll(
      async () =>
        action.evaluate((button) => {
          const row = button.parentElement;
          return {
            active: button === document.activeElement,
            tag: row?.tagName,
            className: row?.className,
            opacity: row ? Number(getComputedStyle(row).opacity) : 0,
          };
        }),
      {
        message: "focused action row should be opaque",
      },
    )
    .toMatchObject({ opacity: 1 });
}
