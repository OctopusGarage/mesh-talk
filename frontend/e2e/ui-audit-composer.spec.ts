import type { Locator, Page } from "@playwright/test";
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
  await page.keyboard.press("Escape");

  await page.getByTestId("composer-stickers").click();
  await expect(page.getByTestId("sticker-panel")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="sticker-panel"]', "body");
  await expectDesignFocusRing(page, page.getByTestId("sticker-option-1f602"));
  await page.keyboard.press("Escape");

  await page.getByTestId("composer-screenshot").click();
  await expect(page.getByTestId("screenshot-menu")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="screenshot-menu"]', "body");
  await expectDesignFocusRing(page, page.getByTestId("screenshot-now"));
  await expectNoHorizontalOverflow(page, "composer popovers");
});

async function expectDesignFocusRing(page: Page, locator: Locator) {
  await expect(locator).toBeVisible();
  await locator.scrollIntoViewIfNeeded();
  await page.evaluate(() => {
    const active = document.activeElement;
    if (active instanceof HTMLElement) active.blur();
  });
  for (let i = 0; i < 80; i += 1) {
    if (await locator.evaluate((el) => el === document.activeElement)) break;
    await page.keyboard.press("Tab");
  }
  const style = await locator.evaluate((el) => {
    const computed = getComputedStyle(el);
    return {
      active: el === document.activeElement,
      boxShadow: computed.boxShadow,
    };
  });

  expect(style.active).toBe(true);
  expect(
    style.boxShadow,
    "focused menu item should use the app focus ring",
  ).not.toBe("none");
}
