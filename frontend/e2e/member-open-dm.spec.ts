import { test, expect } from "./tauri-mock";
test.use({ viewport: { width: 1100, height: 800 } });

const CHANNEL = "chan_team_dddd4444";
const BOB_DEVICE = "device_bob_2222";

// Tapping a channel member opens a 1:1 chat with them.
test("clicking a channel member opens their private chat", async ({ page }) => {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();

  await page.getByTestId(`conversation-row-${CHANNEL}`).click();
  await page.getByTestId("members-trigger").click();
  await page.getByTestId(`member-dm-${BOB_DEVICE}`).click();

  // The members dialog closed and bob's DM is now the active conversation.
  await expect(page.getByTestId("members-trigger")).toBeHidden();
  await expect(page.getByTestId("conversation-header")).toContainText("bob");
});
