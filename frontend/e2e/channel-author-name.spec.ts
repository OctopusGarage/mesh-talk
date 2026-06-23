import { test, expect } from "./tauri-mock";
const CHANNEL = "chan_team_dddd4444";
test.use({ viewport: { width: 1100, height: 800 } });
test("channel message shows the author NAME, not the raw id", async ({
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
  await page.getByTestId(`conversation-row-${CHANNEL}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  // Bob authored "channel kickoff" — the bubble must label him "bob", not "device_bob_2222".
  const bubble = page
    .getByTestId("message-bubble")
    .filter({ hasText: "channel kickoff" });
  await expect(bubble).toContainText("bob");
  await expect(bubble).not.toContainText("device_bob");
});
