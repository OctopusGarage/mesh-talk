import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, seedThemeBeforeLoad } from "./helpers/session";
import { prepareForScreenshot, snapshotName } from "./helpers/visual-snapshot";

const THEMES = [
  "light",
  "dark",
  "oled",
  "argentina",
  "barcelona",
  "messi",
  "nature",
] as const;

const VIEWPORTS = [
  { width: 1280, height: 800 },
  { width: 760, height: 620 },
] as const;

test.describe.configure({ mode: "serial" });

test("first-run sign-in keeps a quiet, bounded surface", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await expect(page.getByTestId("login-form")).toBeVisible();
  await prepareForScreenshot(page);
  await expect(page).toHaveScreenshot(snapshotName("login", "dark", 1280, 800));
});

test("no peers offers a direct connection path", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.evaluate(() => {
    (
      window as unknown as { __mockClearRoster: () => void }
    ).__mockClearRoster();
  });
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await expect(page.getByText("Connection help")).toBeVisible();
  await prepareForScreenshot(page);
  await expect(page.getByTestId("chat-shell")).toHaveScreenshot(
    snapshotName("no-peers", "dark", 1280, 800),
  );
  await page.getByText("Connection help").click();
  await expect(page.getByTestId("diagnostics-dialog")).toBeVisible();
  await expect(page.getByTestId("diagnostics-tab-help")).toHaveAttribute(
    "data-state",
    "active",
  );
});

test("empty conversation keeps the composer in view", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await enterChat(page);
  await openBobDm(page);
  await page.getByTestId("conversation-history-trigger").click();
  await page.getByTestId("clear-history").click();
  await page.getByTestId("clear-history-confirm").click();
  await expect(page.getByTestId("conversation-empty")).toBeVisible();
  await expect(page.getByTestId("composer-input")).toBeVisible();
  await prepareForScreenshot(page);
  await expect(page.getByTestId("chat-shell")).toHaveScreenshot(
    snapshotName("conversation-empty", "dark", 1280, 800),
  );
});

test("failed send keeps retry visible beside the message", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(() => {
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("owner_enqueue_text");
  });
  await page
    .getByTestId("composer-input")
    .fill("Please send this when you can");
  await page.getByTestId("composer-send").click();
  await expect(
    page.getByText("Couldn't send — network unreachable"),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Retry" })).toBeVisible();
  await prepareForScreenshot(page);
  await expect(page.getByTestId("chat-shell")).toHaveScreenshot(
    snapshotName("failed-send", "dark", 1280, 800),
  );
});

for (const viewport of VIEWPORTS) {
  test.describe(`shell matrix ${viewport.width}x${viewport.height}`, () => {
    test.use({ viewport });

    for (const theme of THEMES) {
      test(`shell ${theme}`, async ({ page }) => {
        await seedThemeBeforeLoad(page, theme);
        await enterChat(page);
        await prepareForScreenshot(page);
        await openBobDm(page);

        const shell = page.getByTestId("chat-shell");
        await expect(shell).toBeVisible();
        await expect(shell).toHaveScreenshot(
          snapshotName("chat-shell", theme, viewport.width, viewport.height),
          // Detailed artwork rasterizes differently on Linux and macOS at compact size.
          ["argentina", "barcelona", "messi", "nature"].includes(theme) &&
            viewport.width === 760
            ? { maxDiffPixelRatio: 0.04 }
            : {},
        );
      });
    }
  });
}
