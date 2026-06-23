import { test, expect } from "./tauri-mock";

// A channel shows a WeChat-style composite group avatar: one tile per member (self + bob +
// carol = 3), derived live from the channel members — not a generic icon.
const CHANNEL = "chan_team_dddd4444";
test.use({ viewport: { width: 1100, height: 800 } });

test("channel renders a composite group avatar (one tile per member)", async ({
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

  // The header's group avatar tiles all 3 members once the roster cache loads.
  const tiles = page
    .getByTestId("conversation-header")
    .getByTestId("group-avatar")
    .locator("> *");
  await expect.poll(async () => tiles.count()).toBe(3);
});
