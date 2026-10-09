import { resolve } from "node:path";
import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";

test.skip(process.env.MESH_TALK_VARIANT !== "lite", "Lite build only");

test("lite starts without avatar and theme libraries but accepts ZIPs", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(page.getByTestId("theme-dark")).toBeVisible();
  await expect(page.getByTestId("theme-barcelona")).toHaveCount(0);
  await page
    .getByTestId("pack-manager-theme")
    .locator('input[type="file"]')
    .setInputFiles(resolve("../site/market/packs/barcelona.zip"));
  await expect(page.getByTestId("theme-barcelona")).toBeVisible();

  await page.keyboard.press("Escape");
  await page.getByTestId("conversation-row-chan_team_dddd4444").click();
  await page.getByRole("button", { name: "Change group photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await expect(
    page.getByTestId("avatar-gallery").locator("button"),
  ).toHaveCount(0);
  await page
    .getByTestId("pack-manager-avatar")
    .locator('input[type="file"]')
    .setInputFiles(resolve("../site/market/packs/clubs.zip"));
  await expect(
    page.getByRole("button", { name: "Football clubs", exact: true }),
  ).toBeVisible();
});
