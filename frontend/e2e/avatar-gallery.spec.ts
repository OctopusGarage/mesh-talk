import { waitForChatShell } from "./helpers/session";
import { test, expect } from "./tauri-mock";
import { seedMarketPacks, waitForPackBootstrap } from "./helpers/packs";
const CHANNEL = "chan_team_dddd4444";
test.use({ viewport: { width: 1100, height: 800 } });

async function login(page: import("@playwright/test").Page) {
  await page.goto("/");
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await waitForChatShell(page);
  await waitForPackBootstrap(page);
}

test("personal avatar gallery offers the player pack", async ({ page }) => {
  await seedMarketPacks(page, ["players"]);
  await login(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await expect(page.getByTestId("avatar-gallery")).toBeVisible();
  await page.getByRole("button", { name: "Football", exact: true }).click();
  const tiles = page.getByTestId("avatar-gallery").locator("button");
  await expect.poll(async () => tiles.count()).toBe(18); // 18 players
});

test("personal avatar gallery offers all Cat Acrylic portraits", async ({
  page,
}) => {
  await seedMarketPacks(page, ["cat-acrylic-avatars"]);
  await login(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await page.getByRole("button", { name: "Cat Acrylic", exact: true }).click();
  const gallery = page.getByTestId("avatar-gallery");
  await expect(gallery.locator("button")).toHaveCount(50);
  await gallery.getByRole("button", { name: "Cat 01" }).click();
  await expect(gallery).toBeHidden();
});

test("installed avatar tabs retain their translated names", async ({
  page,
}) => {
  await page.addInitScript(() => localStorage.setItem("mesh-talk-lang", "ja"));
  await seedMarketPacks(page, ["players"]);
  await login(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "自分の写真を変更" }).click();
  await page.getByText("ギャラリーから選択…").click();
  await expect(
    page.getByRole("button", { name: "サッカー", exact: true }),
  ).toBeVisible();
});

test("group avatar gallery offers the club pack", async ({ page }) => {
  await seedMarketPacks(page, ["clubs"]);
  await login(page);
  await page.getByTestId(`conversation-row-${CHANNEL}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await page.getByRole("button", { name: "Change group photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await expect(page.getByTestId("avatar-gallery")).toBeVisible();
  await page.getByRole("button", { name: "Clubs", exact: true }).click();
  await expect
    .poll(async () =>
      page.getByTestId("avatar-gallery").locator("button").count(),
    )
    .toBeGreaterThanOrEqual(19);
  for (const club of [
    "Inter Miami CF",
    "Tottenham Hotspur",
    "Newcastle United",
    "AS Roma",
    "Napoli",
  ]) {
    await expect(
      page.getByTestId("avatar-gallery").getByRole("button", { name: club }),
    ).toBeVisible();
  }
});
