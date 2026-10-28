import { test, expect } from "./tauri-mock";
import {
  waitForChatShell,
  enterChat,
  openBobDm,
  seedThemeBeforeLoad,
} from "./helpers/session";
import {
  prepareForScreenshot,
  snapshotName,
  windowsSnapshotProfile,
} from "./helpers/visual-snapshot";

const THEMES = [
  "light",
  "dark",
  "oled",
  "argentina",
  "barcelona",
  "messi",
  "nature",
  "cat-acrylic",
] as const;

const VIEWPORTS = [
  { width: 1280, height: 800 },
  { width: 760, height: 620 },
] as const;

test("first-run sign-in keeps a quiet, bounded surface", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await expect(page.getByTestId("login-form")).toBeVisible();
  await prepareForScreenshot(page);
  await expect(page).toHaveScreenshot(snapshotName("login", "dark", 1280, 800));
});

for (const theme of ["light", "dark"] as const) {
  test(`first-run registration remains readable in a short ${theme} window`, async ({
    page,
  }) => {
    await seedThemeBeforeLoad(page, theme);
    await page.setViewportSize({ width: 760, height: 520 });
    await page.goto("/");
    await page.getByTestId("login-tab-register").click();
    await expect(page.getByTestId("login-stay-signed-in")).toBeEnabled();
    await expect(page.getByTestId("login-submit")).toBeInViewport();
    await prepareForScreenshot(page);
    const name = snapshotName("register", theme, 760, 520);
    // Native system font metrics change wrapping and form height in this short
    // viewport. Reuse reviewed platform/font references without relaxing the diff.
    const profile =
      process.platform === "win32"
        ? await windowsSnapshotProfile(page.locator("#password-guidance"))
        : process.platform;
    const reference = ["linux", "win32"].includes(process.platform)
      ? name.replace(".png", `-${profile}.png`)
      : name;
    await expect(page).toHaveScreenshot(reference);
  });
}

test("no peers offers a direct connection path", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.evaluate(() => {
    (
      window as unknown as { __mockClearRoster: () => void }
    ).__mockClearRoster();
  });
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await waitForChatShell(page);
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
  await expect(page.getByTestId("clear-history-dialog")).toBeHidden();
  await expect(page.getByTestId("conversation-history-dialog")).toBeHidden();
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
        if (!["light", "dark", "oled"].includes(theme)) {
          await expect(page.locator("html")).toHaveAttribute(
            "data-palette",
            theme,
          );
          await expect
            .poll(() =>
              page.evaluate(async (id) => {
                const path = "/src/store/packs.ts";
                const { installedTheme } = await import(
                  /* @vite-ignore */ path
                );
                return installedTheme(id)?.id;
              }, theme),
            )
            .toBe(theme);
        }
        await prepareForScreenshot(page);
        await openBobDm(page);

        const shell = page.getByTestId("chat-shell");
        await expect(shell).toBeVisible();
        const name = snapshotName(
          "chat-shell",
          theme,
          viewport.width,
          viewport.height,
        );
        // Compact shells expose native font metrics across enough pixels to need
        // reviewed platform references; keep the same strict comparison threshold.
        const profile =
          process.platform === "win32" &&
          theme === "oled" &&
          viewport.width === 760
            ? await windowsSnapshotProfile(
                page.getByText("hey, welcome to the mesh", { exact: true }),
              )
            : process.platform;
        const needsPlatformReference =
          viewport.width === 760 &&
          ["light", "dark", "oled"].includes(theme) &&
          ["linux", "win32"].includes(process.platform);
        const reference = needsPlatformReference
          ? name.replace(".png", `-${profile}.png`)
          : name;
        await expect(shell).toHaveScreenshot(
          reference,
          // Detailed artwork rasterizes differently on Linux and macOS at compact size.
          ["argentina", "barcelona", "messi", "nature", "cat-acrylic"].includes(
            theme,
          ) && viewport.width === 760
            ? { maxDiffPixelRatio: 0.04 }
            : {},
        );
      });
    }
  });
}

for (const theme of ["light", "dark"] as const) {
  for (const viewport of [
    { width: 1280, height: 800 },
    { width: 760, height: 520 },
  ]) {
    test(`unread review ${theme} ${viewport.width}x${viewport.height}`, async ({
      page,
    }) => {
      await seedThemeBeforeLoad(page, theme);
      await page.setViewportSize(viewport);
      await enterChat(page);
      await page.evaluate(() => {
        const emit = (
          window as unknown as {
            __mockEmit: (event: string, payload: unknown) => void;
          }
        ).__mockEmit;
        emit("dm-received", {
          from: "device_bob_2222",
          from_name: "bob",
          text: "An unread message",
          message_id: "review-dm",
          from_account: "acc_bob_bbbb2222",
          reply_to: null,
        });
        emit("dm-received", {
          from: "sender-not-in-directory",
          from_name: "Unknown",
          text: "Waiting for discovery",
          reply_to: null,
        });
      });
      await page.getByTestId("unread-review-trigger").click();
      const review = page.getByTestId("unread-review");
      await expect(
        review.getByText("Waiting to identify sender", { exact: true }),
      ).toBeVisible();
      await expect(
        review.getByRole("button", { name: "Mark all as read", exact: true }),
      ).toBeInViewport();
      expect(
        await review.evaluate(
          (element) => element.scrollWidth <= element.clientWidth + 1,
        ),
      ).toBe(true);
      await prepareForScreenshot(page);
      const name = snapshotName(
        "unread-review",
        theme,
        viewport.width,
        viewport.height,
      );
      const reference = ["linux", "win32"].includes(process.platform)
        ? name.replace(".png", `-${process.platform}.png`)
        : name;
      await expect(review).toHaveScreenshot(reference);
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("unread-review-trigger")).toBeFocused();
    });
  }
}
