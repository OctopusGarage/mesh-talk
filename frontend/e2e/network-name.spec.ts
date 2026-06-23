import { test, expect } from "./tauri-mock";
test.use({ viewport: { width: 1100, height: 800 } });
test("sidebar footer shows the Wi-Fi network name", async ({ page }) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await expect(page.getByText("TestNet-5G")).toBeVisible();
});
