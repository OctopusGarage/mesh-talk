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

test("clicking your avatar opens the profile and lets you rename inline", async ({
  page,
}) => {
  await login(page);
  // Starts as the login username (no nickname set yet).
  await expect(page.getByTestId("sidebar-own-name")).toHaveText("tester");

  // Tap the avatar → profile dialog (WeChat/Telegram pattern).
  await page.getByTestId("open-profile").click();
  await expect(page.getByTestId("profile-dialog")).toBeVisible();
  // The login username is shown read-only so it's never confused with the nickname.
  await expect(page.getByTestId("profile-username")).toHaveText("tester");

  // Click the name to edit it inline.
  await page.getByTestId("profile-name").click();
  const input = page.getByTestId("profile-name-input");
  await expect(input).toHaveValue("tester");
  await input.fill("老王 🐻"); // spaces + unicode + emoji are allowed
  await page.getByTestId("profile-name-save").click();

  // The store updates from the command's returned user, so both the profile and the
  // sidebar identity reflect it.
  await expect(page.getByTestId("profile-name-text")).toHaveText("老王 🐻");
  await expect(page.getByTestId("sidebar-own-name")).toHaveText("老王 🐻");
  // The login username is unchanged by the rename — still shown for sign-in.
  await expect(page.getByTestId("profile-username")).toHaveText("tester");
});
