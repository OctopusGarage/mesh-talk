import { test, expect } from "./tauri-mock";

// Regression: a channel member's propagated/custom avatar must show — both in the members
// list and on their messages. Avatars are keyed by ACCOUNT id, but channel members/authors
// are identified by DEVICE id, so the crest/author glyph must resolve device→account
// (via the roster) or the avatar never appears. The mock gives Bob (acc_bob) a photo.
const CHANNEL = "chan_team_dddd4444";
test.use({ viewport: { width: 1100, height: 800 } });

test("channel member + message-author avatars resolve (device→account)", async ({
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

  // (1) Bob authored "channel kickoff" — his message-author glyph should be his avatar <img>.
  await expect
    .poll(async () =>
      page.evaluate(() => {
        const row = [
          ...document.querySelectorAll('[data-testid="message-bubble"]'),
        ].find((r) => r.textContent?.includes("channel kickoff"));
        return !!row?.querySelector("img");
      }),
    )
    .toBe(true);

  // (2) Bob's row in the members list should show his avatar <img>, not the glyph.
  await page.getByTestId("members-trigger").click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect
    .poll(async () =>
      page.evaluate(() => {
        const row = [
          ...document.querySelectorAll('[role="dialog"] .group'),
        ].find((r) => r.textContent?.includes("bob"));
        return !!row?.querySelector("img");
      }),
    )
    .toBe(true);
});
