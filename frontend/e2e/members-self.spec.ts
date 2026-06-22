import { test, expect } from "./tauri-mock";

// Regression: the channel members list must show OUR OWN row with our display name and an
// online status. We never appear in our own discovery roster, so the backend falls back to
// our raw user_id for the name and there is no presence entry for us — the UI has to fill in
// our name + online state. Previously the self row showed the hex user_id and "offline".
const CHANNEL = "chan_team_dddd4444";
const SELF_DEVICE_ID = "device_self_0001"; // the raw user_id the backend falls back to

test.use({ viewport: { width: 1280, height: 800 } });

test("channel members shows self with name + online", async ({ page }) => {
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

  await page.getByTestId("members-trigger").click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await page.screenshot({ path: "e2e/__screens__/members-self.png" });

  // Self shows our display name, not the raw hex user_id.
  const selfRow = dialog.locator(".group").filter({ hasText: "tester" });
  await expect(selfRow).toHaveCount(1);
  await expect(dialog).not.toContainText(SELF_DEVICE_ID);

  // The self row is shown online (scoped to our row so it's not Bob's presence).
  await expect(selfRow.getByLabel("online")).toBeVisible();
});
