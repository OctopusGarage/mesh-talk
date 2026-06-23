import { test, expect } from "./tauri-mock";
test.use({ viewport: { width: 1100, height: 800 } });

async function login(page: import("@playwright/test").Page) {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
}
const palette = (page: import("@playwright/test").Page) =>
  page.evaluate(() => document.documentElement.getAttribute("data-palette"));

test("theme picker applies + persists a brand palette, and clears it for base modes", async ({
  page,
}) => {
  await login(page);
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-settings").click();
  await expect(page.getByTestId("theme-picker")).toBeVisible();

  await page.getByTestId("theme-barcelona").click();
  await expect.poll(() => palette(page)).toBe("barcelona");
  expect(
    await page.evaluate(() => localStorage.getItem("mesh-talk-theme")),
  ).toBe("barcelona");
  // The brand crest surfaces in the footer mark while the theme is active.
  await expect(page.getByTestId("theme-crest")).toBeVisible();

  await page.getByTestId("theme-dark").click();
  await expect.poll(() => palette(page)).toBe(null); // base mode clears the palette
  await expect(page.getByTestId("theme-crest")).toBeHidden(); // back to the app mark
});
