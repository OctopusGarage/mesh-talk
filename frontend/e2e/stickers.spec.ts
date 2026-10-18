import { test, expect } from "./tauri-mock";
import { resolve } from "node:path";
import { readFileSync } from "node:fs";
import { strToU8, unzipSync, zipSync } from "fflate";
import type { Page } from "@playwright/test";
import { revealComposerTools } from "./helpers/session";
import { waitForPackBootstrap } from "./helpers/packs";

const BOB = { account: "acc_bob_bbbb2222" };

async function enterBobDm(page: Page) {
  test.setTimeout(
    process.platform === "win32" || process.env.MESH_TALK_E2E_COVERAGE === "1"
      ? 90_000
      : 60_000,
  );
  await page.goto("/");
  if (!(await page.getByTestId("chat-shell").isVisible())) {
    await page.getByTestId("login-tab-signin").click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible({ timeout: 30_000 });
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByText("hey, welcome to the mesh")).toBeVisible();
  await waitForPackBootstrap(page);
}

test.use({ viewport: { width: 1100, height: 800 } });

test("open the sticker panel and send an animated sticker", async ({
  page,
}) => {
  test.skip(process.env.MESH_TALK_VARIANT === "lite", "Default edition only");
  await enterBobDm(page);

  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  const panel = page.getByTestId("sticker-panel");
  await expect(panel).toBeVisible();
  await expect(page.getByTestId("sticker-pack-google-noto")).toBeVisible();
  await expect(
    panel.locator('[data-testid^="sticker-option-pack:google-noto:"]'),
  ).toHaveCount(40);
  await expect(panel.locator("img").first()).toBeVisible();
  await expect
    .poll(() =>
      panel
        .locator("img")
        .first()
        .evaluate((img) => (img as HTMLImageElement).naturalWidth),
    )
    .toBeGreaterThan(0);

  // Send the "face with tears of joy" sticker from the removable pack.
  await page.getByTestId("sticker-option-pack:google-noto:1f602").click();
  // Panel closes and the sticker lands as its own bubble-less message.
  await expect(panel).toBeHidden();
  await expect(page.getByTestId("message-sticker").last()).toBeVisible();
});

test("lite starts without Google Noto and can install it", async ({ page }) => {
  test.skip(process.env.MESH_TALK_VARIANT !== "lite", "Lite edition only");
  await enterBobDm(page);
  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await expect(page.getByTestId("sticker-pack-google-noto")).toHaveCount(0);
  await page.getByText("Manage sticker packs").click();
  await page
    .getByTestId("pack-manager-sticker")
    .locator('input[type="file"]')
    .setInputFiles(resolve("../site/market/packs/google-noto.zip"));
  await page.getByTestId("sticker-pack-google-noto").click();
  await expect(
    page.getByTestId("sticker-option-pack:google-noto:1f602"),
  ).toBeVisible();
});

test("the default edition installs the cat sticker collection", async ({
  page,
}) => {
  test.skip(process.env.MESH_TALK_VARIANT === "lite", "Default edition only");
  await enterBobDm(page);
  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await page.getByTestId("sticker-pack-cats").click();
  const panel = page.getByTestId("sticker-panel");
  await expect(
    panel.locator('[data-testid^="sticker-option-pack:cats:"]'),
  ).toHaveCount(45);
  const option = page.getByTestId("sticker-option-pack:cats:cat-01-01");
  await expect(option.locator("img")).toBeVisible();
  await option.click();
  await expect(page.getByTestId("message-sticker").last()).toHaveAttribute(
    "alt",
    "Cool cat",
  );
});

test("install and remove a sticker ZIP with fallback for sent messages", async ({
  page,
}) => {
  const files = unzipSync(
    readFileSync(resolve("../site/market/packs/google-noto.zip")),
  );
  const manifest = JSON.parse(new TextDecoder().decode(files["manifest.json"]));
  manifest.id = "imported-noto";
  manifest.name = "Imported Noto";
  files["manifest.json"] = strToU8(JSON.stringify(manifest));
  await enterBobDm(page);
  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await page.getByText("Manage sticker packs").click();
  const manager = page.getByTestId("pack-manager-sticker");
  // A distinct ID keeps bundled seeding separate from the manual import lifecycle.
  const remove = manager.getByRole("button", { name: "Remove Imported Noto" });
  await expect(remove).toHaveCount(0);
  await manager.locator('input[type="file"]').setInputFiles({
    name: "imported-noto.zip",
    mimeType: "application/zip",
    buffer: Buffer.from(zipSync(files)),
  });
  // This validates every image in a large archive before committing it.
  await expect(remove).toBeVisible({
    timeout: 30_000,
  });
  await page.getByTestId("sticker-pack-imported-noto").click();
  await page.getByTestId("sticker-option-pack:imported-noto:1f602").click();
  await expect(page.getByTestId("message-sticker").last()).toHaveAttribute(
    "src",
    /^data:image\/webp;base64,/,
  );

  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await page.getByText("Manage sticker packs").click();
  await page
    .getByTestId("pack-manager-sticker")
    .getByRole("button", { name: "Remove Imported Noto" })
    .click();
  await expect(
    page.getByTestId("sticker-option-pack:imported-noto:1f602"),
  ).toHaveCount(0);
  await expect(page.getByTestId("message-sticker-fallback").last()).toHaveText(
    "😂",
  );
  await page.reload();
  await enterBobDm(page);
  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await expect(page.getByTestId("sticker-pack-imported-noto")).toHaveCount(0);
});

test("a custom noto pack has its own sticker tab", async ({ page }) => {
  const files = unzipSync(
    readFileSync(resolve("../site/market/packs/google-noto.zip")),
  );
  const manifest = JSON.parse(new TextDecoder().decode(files["manifest.json"]));
  manifest.id = "noto";
  manifest.name = "Custom Noto";
  files["manifest.json"] = strToU8(JSON.stringify(manifest));
  await enterBobDm(page);
  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await page.getByText("Manage sticker packs").click();
  await page
    .getByTestId("pack-manager-sticker")
    .locator('input[type="file"]')
    .setInputFiles({
      name: "noto.zip",
      mimeType: "application/zip",
      buffer: Buffer.from(zipSync(files)),
    });
  await page.getByTestId("sticker-pack-noto").click();
  await expect(
    page.getByTestId("sticker-option-pack:noto:1f602"),
  ).toBeVisible();
  if (process.env.MESH_TALK_VARIANT !== "lite") {
    await page.getByTestId("sticker-pack-google-noto").click();
    await expect(
      page.getByTestId("sticker-option-pack:google-noto:1f602"),
    ).toBeVisible();
  }
});

test("installs a sticker from the verified marketplace catalog", async ({
  page,
}) => {
  const catalog = JSON.parse(
    readFileSync(resolve("../site/market/catalog.json"), "utf8"),
  ) as { id: string }[];
  const entry = catalog.find((item) => item.id === "google-noto");
  if (!entry) throw new Error("Missing sticker catalog entry");
  await page.route("**/marketplace/catalog.json", (route) =>
    route.fulfill({
      contentType: "application/json",
      headers: { "access-control-allow-origin": "*" },
      body: JSON.stringify({ schemaVersion: 1, packs: [entry] }),
    }),
  );
  await page.route("**/marketplace/packs/google-noto/*/*.zip", (route) =>
    route.fulfill({
      contentType: "application/zip",
      headers: { "access-control-allow-origin": "*" },
      body: readFileSync(resolve("../site/market/packs/google-noto.zip")),
    }),
  );
  await enterBobDm(page);
  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await page.getByText("Manage sticker packs").click();
  const manager = page.getByTestId("pack-manager-sticker");
  await manager.getByRole("button", { name: /^(Install|Reinstall)$/ }).click();
  await page.getByTestId("sticker-pack-google-noto").click();
  await expect(
    page.getByTestId("sticker-option-pack:google-noto:1f602"),
  ).toBeVisible();
});

test("composer popovers dismiss on an outside click (no second button press)", async ({
  page,
}) => {
  await enterBobDm(page);

  // Emoji picker: open, then click elsewhere (the message log) → it closes by itself.
  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await expect(page.getByTestId("emoji-picker")).toBeVisible();
  await page.getByTestId("conversation-header").click();
  await expect(page.getByTestId("emoji-picker")).toBeHidden();

  // Sticker panel: open, then dismiss with Escape.
  await revealComposerTools(page);
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await expect(page.getByTestId("sticker-panel")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("sticker-panel")).toBeHidden();
});
