import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, CHANNEL } from "./helpers/session";

test.use({
  viewport: { width: 1280, height: 800 },
  reducedMotion: "reduce",
});

test.skip(
  process.env.MESH_TALK_CAPTURE_SITE !== "1",
  "Run explicitly when refreshing site and README screenshots.",
);

const output = resolve("../tmp/site-captures");

test("capture current desktop surfaces for the site and README", async ({
  page,
}) => {
  await mkdir(output, { recursive: true });
  await enterChat(page);

  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await page
    .getByTestId("composer-input")
    .fill("Field notes are ready for the team.");
  await page.getByTestId("composer-input").press("Enter");
  await expect(
    page.getByRole("log").getByText("Field notes are ready for the team."),
  ).toBeVisible();
  await page.screenshot({ path: resolve(output, "hero-channel.png") });

  await openBobDm(page);
  await page.getByTestId("verify-trigger").click();
  await expect(page.getByTestId("verify-dialog")).toBeVisible();
  await page.screenshot({ path: resolve(output, "verify.png") });
  await page.keyboard.press("Escape");

  await page.getByTestId("sidebar-nav-settings").click();
  await expect(page.getByTestId("settings-dialog")).toBeVisible();
  await page.screenshot({ path: resolve(output, "settings.png") });
  await expect(page.getByTestId("theme-picker")).toBeVisible();
  await page.getByTestId("theme-barcelona").click();
  await page.screenshot({ path: resolve(output, "themes.png") });
  await page.getByTestId("theme-dark").click();
  await page.keyboard.press("Escape");

  await page.getByTestId("composer-more-tools").click();
  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await expect(page.getByTestId("sticker-panel")).toBeVisible();
  await page.screenshot({ path: resolve(output, "stickers.png") });
  await page.keyboard.press("Escape");

  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await expect(page.getByTestId("avatar-gallery")).toBeVisible();
  await page.screenshot({ path: resolve(output, "avatars-players.png") });
  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");

  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByRole("button", { name: "Change group photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await expect(page.getByTestId("avatar-gallery")).toBeVisible();
  await page.screenshot({ path: resolve(output, "avatars-clubs.png") });
});
