import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
import {
  expectMinTargetSize,
  expectNoHorizontalOverflow,
} from "./helpers/ui-audit";

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

  await firstBubble
    .getByText("hey, welcome to the mesh")
    .click({ button: "right" });
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
