import { test, expect } from "./tauri-mock";
test.use({ viewport: { width: 1100, height: 800 } });
test("footer counts only presence-ONLINE people, not every roster entry", async ({
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
  const count = page.getByTestId("lan-online-count");
  await expect(count).toBeVisible();
  // Mock roster = bob + carol, but only bob is presence-ONLINE (carol last seen 120s ago).
  // The count must reflect the online dot (1), NOT raw roster membership (2).
  await expect.poll(async () => (await count.textContent())?.trim()).toBe("1");
});
