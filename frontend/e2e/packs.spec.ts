import { resolve } from "node:path";
import { readFileSync } from "node:fs";
import { strToU8, zipSync } from "fflate";
import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";

test.skip(process.env.MESH_TALK_VARIANT === "lite", "Default build only");

test("default build preinstalls a theme and keeps its removal after restart", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const manager = page.getByTestId("pack-manager-theme");
  await expect(page.getByTestId("theme-barcelona")).toBeVisible();
  await page.getByTestId("theme-barcelona").click();
  await expect(page.locator("html")).toHaveAttribute(
    "data-palette",
    "barcelona",
  );
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(page.getByTestId("theme-barcelona")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await manager.getByRole("button", { name: "Remove Barcelona" }).click();
  await expect(page.locator("html")).not.toHaveAttribute(
    "data-palette",
    "barcelona",
  );
  await expect(page.getByTestId("theme-dark")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(page.getByTestId("theme-barcelona")).toHaveCount(0);
});

test("installs a channel avatar library from ZIP", async ({ page }) => {
  await enterChat(page);
  await page.getByTestId("conversation-row-chan_team_dddd4444").click();
  await page.getByRole("button", { name: "Change group photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await expect(
    page.getByRole("button", { name: "Football clubs", exact: true }),
  ).toBeVisible();
  await page
    .getByTestId("pack-manager-avatar")
    .getByRole("button", { name: "Remove Football clubs" })
    .click();
  await expect(
    page.getByRole("button", { name: "Football clubs", exact: true }),
  ).toHaveCount(0);
  await page
    .getByTestId("pack-manager-avatar")
    .locator('input[type="file"]')
    .setInputFiles(resolve("../site/market/packs/clubs.zip"));
  await expect(
    page.getByRole("button", { name: "Football clubs", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByTestId("avatar-gallery").locator("button"),
  ).not.toHaveCount(0);
});

test("installs a verified marketplace download", async ({ page }) => {
  const catalog = JSON.parse(
    readFileSync(resolve("../site/market/catalog.json"), "utf8"),
  ) as { id: string }[];
  const player = catalog.find((item) => item.id === "players");
  if (!player) throw new Error("Missing players catalog entry");
  await page.route("**/market/catalog.json", (route) =>
    route.fulfill({
      contentType: "application/json",
      headers: { "access-control-allow-origin": "*" },
      body: JSON.stringify([player]),
    }),
  );
  await page.route("**/market/packs/players.zip", (route) =>
    route.fulfill({
      contentType: "application/zip",
      headers: { "access-control-allow-origin": "*" },
      body: readFileSync(resolve("../site/market/packs/players.zip")),
    }),
  );
  await enterChat(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  const manager = page.getByTestId("pack-manager-avatar");
  await manager.getByRole("button", { name: "Remove Football stars" }).click();
  await manager.getByRole("button", { name: "Install", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Football stars", exact: true }),
  ).toBeVisible();
});

test("rejects an image that has a PNG signature but cannot be decoded", async ({
  page,
}) => {
  const zip = zipSync({
    "manifest.json": strToU8(
      JSON.stringify({
        format: 1,
        id: "broken.image",
        version: "1.0.0",
        name: "Broken image",
        kind: "avatar",
        category: "personal",
        fit: "cover",
        avatars: [{ label: "Broken", file: "images/broken.png" }],
      }),
    ),
    "images/broken.png": new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10, 0]),
  });
  await enterChat(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  const manager = page.getByTestId("pack-manager-avatar");
  await manager.locator('input[type="file"]').setInputFiles({
    name: "broken.zip",
    mimeType: "application/zip",
    buffer: Buffer.from(zip),
  });
  await expect(manager.getByRole("alert")).toContainText("cannot be decoded");
  await expect(manager.getByText("Broken image")).toHaveCount(0);
});
