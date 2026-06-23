import { test, expect } from "./tauri-mock";
test.use({ viewport: { width: 1100, height: 800 } });

const BOB = "acc_bob_bbbb2222";

// Native-app feel: UI chrome must not be drag-selectable like a web page, but real content
// (message text) and form fields stay selectable.
test("chrome is unselectable; message text and inputs stay selectable", async ({
  page,
}) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();

  const sel = (el: import("@playwright/test").Locator) =>
    el.evaluate((n) => getComputedStyle(n).userSelect);

  // Chrome inherits user-select: none from the body.
  expect(
    await page.evaluate(() => getComputedStyle(document.body).userSelect),
  ).toBe("none");

  await page.getByTestId(`conversation-row-${BOB}`).click();

  // The composer textarea stays selectable.
  expect(await sel(page.getByTestId("composer-input"))).toBe("text");

  // A message's text body stays selectable (bob's DM has seeded messages).
  const body = page.locator(".select-text").first();
  await expect(body).toBeVisible();
  expect(await sel(body)).toBe("text");
});
