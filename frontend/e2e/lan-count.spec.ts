import { test, expect } from "./tauri-mock";
test.use({ viewport: { width: 1100, height: 800 } });
test("footer shows the live LAN online-people count", async ({ page }) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  const count = page.getByTestId("lan-online-count");
  await expect(count).toBeVisible();
  // mock peers = bob + carol → 2 distinct accounts online.
  await expect.poll(async () => (await count.textContent())?.trim()).toBe("2");
});
