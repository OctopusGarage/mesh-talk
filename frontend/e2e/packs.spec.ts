import { resolve } from "node:path";
import { readFileSync } from "node:fs";
import { strToU8, zipSync } from "fflate";
import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { revealComposerTools } from "./helpers/session";
import { seedMarketPacks } from "./helpers/packs";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";

test.skip(process.env.MESH_TALK_VARIANT === "lite", "Default build only");

test("a missing bundled ZIP does not hide an installed sticker pack", async ({
  page,
}) => {
  await page.route("**/builtin-packs/nature.zip", (route) =>
    route.fulfill({ status: 404, body: "missing" }),
  );
  await seedMarketPacks(page, ["noto-favorites"]);
  await enterChat(page);
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await expect(
    page.getByTestId("sticker-option-pack:noto-favorites:1f602"),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await page
    .getByTestId("pack-manager-theme")
    .getByRole("button", { name: "Remove Barcelona" })
    .click();
  await expect(page.getByTestId("theme-barcelona")).toHaveCount(0);
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(page.getByTestId("theme-barcelona")).toHaveCount(0);
  await page.unroute("**/builtin-packs/nature.zip");
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(page.getByTestId("theme-nature")).toBeVisible();
  await expect(page.getByTestId("theme-barcelona")).toHaveCount(0);
});

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
  await expect(
    manager.getByRole("button", { name: "Reinstall", exact: true }),
  ).toBeVisible();
  await manager.getByRole("button", { name: "Remove Football stars" }).click();
  await manager.getByRole("button", { name: "Install", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Football stars", exact: true }),
  ).toBeVisible();
});

test("can retry the marketplace after a temporary catalog failure", async ({
  page,
}) => {
  let requests = 0;
  let recover = false;
  const catalog = JSON.parse(
    readFileSync(resolve("../site/market/catalog.json"), "utf8"),
  ) as { id: string }[];
  await page.route("**/market/catalog.json", (route) => {
    requests += 1;
    return route.fulfill({
      status: recover ? 200 : 503,
      contentType: "application/json",
      headers: { "access-control-allow-origin": "*" },
      body: JSON.stringify(catalog.filter((item) => item.id === "players")),
    });
  });
  await enterChat(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  const manager = page.getByTestId("pack-manager-avatar");
  const retry = manager.getByRole("button", { name: "Retry" });
  await expect(retry).toBeVisible();
  recover = true;
  await retry.click();
  await expect(
    manager.getByRole("button", { name: "Reinstall", exact: true }),
  ).toBeVisible();
  expect(requests).toBeGreaterThanOrEqual(2);
});

test("keeps the existing marketplace usable before versioned catalog deployment", async ({
  page,
}) => {
  const catalog = JSON.parse(
    readFileSync(resolve("../site/market/catalog.json"), "utf8"),
  ) as { id: string; version?: string }[];
  const player = catalog.find((item) => item.id === "players");
  if (!player) throw new Error("Missing players catalog entry");
  const legacy = { ...player };
  delete legacy.version;
  await page.route("**/market/catalog.json", (route) =>
    route.fulfill({
      contentType: "application/json",
      headers: { "access-control-allow-origin": "*" },
      body: JSON.stringify([legacy]),
    }),
  );
  await enterChat(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  const manager = page.getByTestId("pack-manager-avatar");
  await expect(manager.getByRole("button", { name: "Replace" })).toBeVisible();
  await manager.getByRole("button", { name: "Remove Football stars" }).click();
  await manager.getByRole("button", { name: "Install", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Football stars", exact: true }),
  ).toBeVisible();
});

test("rejects a marketplace ZIP whose contents differ from its listing", async ({
  page,
}) => {
  const catalog = JSON.parse(
    readFileSync(resolve("../site/market/catalog.json"), "utf8"),
  ) as { id: string; name: string }[];
  const player = catalog.find((item) => item.id === "players");
  if (!player) throw new Error("Missing players catalog entry");
  await page.route("**/market/catalog.json", (route) =>
    route.fulfill({
      contentType: "application/json",
      headers: { "access-control-allow-origin": "*" },
      body: JSON.stringify([{ ...player, name: "A different listing" }]),
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
  await expect(manager.getByRole("alert")).toContainText(
    "does not match its marketplace listing",
  );
  await expect(
    page.getByRole("button", { name: "Football stars", exact: true }),
  ).toHaveCount(0);
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
