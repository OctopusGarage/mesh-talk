import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { test, expect } from "./tauri-mock";
import {
  enterChat,
  BOB,
  CHANNEL,
  seedThemeBeforeLoad,
} from "./helpers/session";

test.use({
  viewport: { width: 1280, height: 800 },
  reducedMotion: "reduce",
});

test.skip(
  process.env.MESH_TALK_CAPTURE_SITE !== "1",
  "Run explicitly when refreshing site and README screenshots.",
);

const output = resolve("../tmp/site-captures");

async function waitForThemePaint(page: import("@playwright/test").Page) {
  await page.evaluate(async () => {
    await Promise.all(
      document
        .getAnimations()
        .filter((animation) => animation instanceof CSSTransition)
        .map((animation) => animation.finished.catch(() => {})),
    );
  });
}

test("capture current desktop surfaces for the site and README", async ({
  page,
}) => {
  await mkdir(output, { recursive: true });
  await seedThemeBeforeLoad(page, "messi");
  await enterChat(page, "Cesc Fàbregas", "/?data=site");

  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await expect(
    page
      .getByRole("log")
      .getByText("I'll bring the match balls. See you there! ⚽"),
  ).toBeVisible();
  await page.evaluate(async () => {
    await document.fonts.ready;
    await Promise.all(
      [...document.images].map((img) => img.decode().catch(() => {})),
    );
  });
  await page.screenshot({ path: resolve(output, "hero-messi.png") });

  for (const theme of ["barcelona", "argentina"] as const) {
    await page.getByTestId("sidebar-nav-settings").click();
    await page.getByTestId(`theme-${theme}`).click();
    await expect(page.locator("html")).toHaveAttribute("data-palette", theme);
    await waitForThemePaint(page);
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("settings-dialog")).toBeHidden();
    await page
      .getByTestId("sidebar-nav-settings")
      .evaluate((button) => (button as HTMLElement).blur());
    await page.mouse.move(1000, 110);
    await page.screenshot({ path: resolve(output, `hero-${theme}.png`) });
  }

  await page.getByTestId("sidebar-nav-settings").click();
  await page.getByTestId("theme-messi").click();
  await page.keyboard.press("Escape");

  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await page.getByTestId("verify-trigger").click();
  await expect(page.getByTestId("verify-dialog")).toBeVisible();
  await page.screenshot({ path: resolve(output, "verify.png") });
  await page.keyboard.press("Escape");

  await page.getByTestId("sidebar-nav-settings").click();
  await expect(page.getByTestId("settings-dialog")).toBeVisible();
  await page.screenshot({ path: resolve(output, "settings.png") });
  await expect(page.getByTestId("theme-picker")).toBeVisible();
  await page.getByTestId("theme-barcelona").click();
  await waitForThemePaint(page);
  await page.screenshot({ path: resolve(output, "themes.png") });
  await page.getByTestId("theme-messi").click();
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
