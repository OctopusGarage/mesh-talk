import { test, expect } from "./tauri-mock";
import type { Page } from "@playwright/test";

const BOB = { account: "acc_bob_bbbb2222" };

async function enterBobDm(page: Page) {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByText("hey, welcome to the mesh")).toBeVisible();
}

test.use({ viewport: { width: 1100, height: 800 } });

test("open the sticker panel and send an animated sticker", async ({ page }) => {
  await enterBobDm(page);

  await page.getByTestId("composer-stickers").click();
  const panel = page.getByTestId("sticker-panel");
  await expect(panel).toBeVisible();
  // The bundled Noto set is present (thumbnails are real animated WebP).
  await expect(panel.locator("img").first()).toBeVisible();

  // Send the "face with tears of joy" sticker (bundled id 1f602).
  await page.getByTestId("sticker-option-1f602").click();
  // Panel closes and the sticker lands as its own bubble-less message.
  await expect(panel).toBeHidden();
  await expect(page.getByTestId("message-sticker").last()).toBeVisible();
});
