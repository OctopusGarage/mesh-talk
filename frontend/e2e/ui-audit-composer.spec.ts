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
  await page.getByTestId("composer-input").fill("composer audit");

  for (const id of [
    "composer-screenshot",
    "composer-attach",
    "composer-image",
    "composer-emoji",
    "composer-stickers",
    "composer-send",
  ]) {
    await expectMinTargetSize(page.getByTestId(id), 36, id);
    await expectVisibleFocus(page, page.getByTestId(id));
  }

  await page.getByTestId("composer-emoji").click();
  await expect(page.getByTestId("emoji-picker")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="emoji-picker"]', "body");
  await expectVisibleFocus(page, page.getByTestId("emoji-option-😀"));
  await page.keyboard.press("Escape");

  await page.getByTestId("composer-stickers").click();
  await expect(page.getByTestId("sticker-panel")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="sticker-panel"]', "body");
  await expectVisibleFocus(page, page.getByTestId("sticker-option-1f602"));
  await page.keyboard.press("Escape");

  await page.getByTestId("composer-screenshot").click();
  await expect(page.getByTestId("screenshot-menu")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="screenshot-menu"]', "body");
  await expectVisibleFocus(page, page.getByTestId("screenshot-now"));
  await expectNoHorizontalOverflow(page, "composer popovers");
});

test.describe("narrow composer pane", () => {
  test.use({ viewport: { width: 760, height: 620 } });

  test("composer popovers fit when a persisted wide sidebar narrows the chat pane", async ({
    page,
  }) => {
    await page.addInitScript(() => {
      localStorage.setItem("mesh-talk-sidebar-width", "460");
    });
    await enterChat(page);
    await openBobDm(page);

    await page.getByTestId("composer-emoji").click();
    await expect(page.getByTestId("emoji-picker")).toBeVisible();
    await expectElementsWithin(page, '[data-testid="emoji-picker"]', "body");
    await expectNoHorizontalOverflow(page, "narrow pane emoji picker");
    await page.keyboard.press("Escape");

    await page.getByTestId("composer-stickers").click();
    await expect(page.getByTestId("sticker-panel")).toBeVisible();
    await expectElementsWithin(page, '[data-testid="sticker-panel"]', "body");
    await expectNoHorizontalOverflow(page, "narrow pane sticker panel");
    await page.keyboard.press("Escape");

    await page.getByTestId("composer-screenshot").click();
    await expect(page.getByTestId("screenshot-menu")).toBeVisible();
    await expectElementsWithin(page, '[data-testid="screenshot-menu"]', "body");
    await expectNoHorizontalOverflow(page, "narrow pane screenshot menu");
  });
});
