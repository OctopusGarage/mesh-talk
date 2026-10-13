import { expect, test } from "./tauri-mock";

test("owner can dissolve a channel and keep its history read-only", async ({
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
  await page.getByTestId("conversation-row-chan_team_dddd4444").click();
  await page.getByTestId("members-trigger").click();
  await page.getByTestId("dissolve-channel-trigger").click();
  await expect(page.getByTestId("dissolve-channel-dialog")).toBeVisible();
  await page.getByTestId("dissolve-channel-confirm").click();
  await expect(page.getByText("Dissolved channel").first()).toBeVisible();
  await expect(page.getByText(/owner dissolved this channel/i)).toBeVisible();
  await expect(page.getByTestId("members-trigger")).toBeVisible();
  await expect(page.getByTestId("dissolve-channel-trigger")).toHaveCount(0);
});
