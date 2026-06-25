import { test, expect } from "./tauri-mock";
const CHANNEL = "chan_team_dddd4444";
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

test("personal avatar gallery offers the player pack", async ({ page }) => {
  await login(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await expect(page.getByTestId("avatar-gallery")).toBeVisible();
  const tiles = page.getByTestId("avatar-gallery").locator("button");
  await expect.poll(async () => tiles.count()).toBe(18); // 18 players
});

test("group avatar gallery offers the club pack", async ({ page }) => {
  await login(page);
  await page.getByTestId(`conversation-row-${CHANNEL}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await page.getByRole("button", { name: "Change group photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await expect(page.getByTestId("avatar-gallery")).toBeVisible();
  await expect
    .poll(async () =>
      page.getByTestId("avatar-gallery").locator("button").count(),
    )
    .toBe(14); // 14 clubs
});
