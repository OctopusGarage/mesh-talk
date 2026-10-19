import { selectSettingsCategory } from "./helpers/settings";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import type { Locator } from "@playwright/test";
import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
import { prepareForScreenshot, snapshotName } from "./helpers/visual-snapshot";
import {
  expectDialogFitsViewport,
  expectMinTargetSize,
} from "./helpers/ui-audit";

test.use({ viewport: { width: 820, height: 620 } });

test("high-frequency dialogs fit the viewport and expose usable close targets", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await prepareForScreenshot(page);

  const cases = [
    {
      open: async () => page.getByTestId("sidebar-action-search").click(),
      surface: "search-dialog",
      hasDialogClose: true,
      ready: async () => {
        await page.getByTestId("search-input").fill("welcome");
        await expect(page.getByTestId("search-result")).toHaveCount(1);
      },
    },
    {
      open: async () => {
        await page.evaluate(() => {
          const emit = (
            window as unknown as { __mockEmit: (e: string, p: unknown) => void }
          ).__mockEmit;
          emit("file-received", {
            from: "device_bob_2222",
            name: "report.pdf",
            size: 5678,
            file_conv: "fc_pdf_1",
            conv: "acc_bob_bbbb2222",
            mime: "application/pdf",
            media: false,
          });
          emit("file-received", {
            from: "device_bob_2222",
            name: "clip.mov",
            size: 9012,
            file_conv: "fc_mov_1",
            conv: "acc_bob_bbbb2222",
            mime: "video/quicktime",
            media: false,
          });
        });
        await page.getByTestId("sidebar-action-files").click();
      },
      surface: "files-tray",
      hasDialogClose: true,
      ready: async () => {
        await expect(page.getByText("report.pdf")).toBeVisible();
        await expect(page.getByText("clip.mov")).toBeVisible();
        await page.getByTestId("files-search").fill("report");
        await expect(page.getByText("clip.mov")).toHaveCount(0);
        await page.getByTestId("files-search").clear();
      },
    },
    {
      open: async () =>
        page.getByTestId("conversation-history-trigger").click(),
      surface: "conversation-history-dialog",
      hasDialogClose: true,
      ready: async () => {
        await expect(page.getByTestId("history-search-input")).toBeVisible();
        await expect(page.getByTestId("history-row")).toHaveCount(2);
      },
    },
    {
      open: async () => {
        await openSidebarMenuAction(page, "sidebar-nav-settings");
      },
      surface: "settings-dialog",
      hasDialogClose: true,
      ready: async () => {
        await expect(page.getByTestId("settings-page-general")).toBeVisible();
        await expect(page.getByTestId("settings-dialog")).toContainText(
          "Manage privacy, appearance, messages, files, and app behavior.",
        );
      },
    },
    {
      open: async () => page.getByTestId("open-profile").click(),
      surface: "profile-dialog",
      hasDialogClose: true,
      ready: async () => {
        await expect(page.getByTestId("profile-username")).toBeVisible();
      },
    },
  ];

  for (const item of cases) {
    await item.open();
    const surface = page.getByTestId(item.surface);
    await expect(surface).toBeVisible();
    await expectDialogFitsViewport(page, item.surface);
    await item.ready?.();
    // The history description can wrap by one line across system fonts; its
    // content and viewport bounds are asserted above instead of pixel matching.
    if (item.surface !== "conversation-history-dialog") {
      await expect(surface).toHaveScreenshot(
        snapshotName(item.surface, "dark", 820, 620),
        { maxDiffPixelRatio: 0.05 },
      );
    }

    if (item.hasDialogClose) {
      await expectSurfaceMotionSettled(surface);
      await expectMinTargetSize(
        surface.getByRole("button", { name: "Close" }),
        32,
        `${item.surface} close`,
      );
      await page.keyboard.press("Escape");
    } else {
      await expectSurfaceMotionSettled(surface);
      await surface.focus();
      await page.keyboard.press("Escape");
    }

    await expect(surface).toHaveCount(0);
  }
});

test("settings page scrolls independently while the header remains visible", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "privacy");
  await expect(page.locator("#setting-privacy-level")).toBeVisible();
  const panel = page.locator("#settings-category-panel");
  await panel.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  await expect
    .poll(() => panel.evaluate((element) => element.scrollTop))
    .toBeGreaterThan(0);
  await expect(
    page
      .getByTestId("settings-dialog")
      .getByRole("heading", { name: "Settings", exact: true }),
  ).toBeInViewport();
  await expect(page.getByTestId("settings-nav-privacy")).toBeInViewport();
});

test("settings section navigation reaches the requested controls", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const dialog = page.getByTestId("settings-dialog");
  await selectSettingsCategory(page, "history");
  await expect(
    dialog.getByTestId("settings-retention-select"),
  ).toBeInViewport();
});

test("settings navigation keeps the dialog header and sidebar in place", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");

  const dialog = page.getByTestId("settings-dialog");
  await selectSettingsCategory(page, "history");

  await expect(
    dialog.getByRole("heading", { name: "Settings" }),
  ).toBeInViewport();
  await expect(page.getByTestId("settings-nav-connection")).toBeInViewport();
  await expect.poll(() => dialog.evaluate((node) => node.scrollTop)).toBe(0);

  await selectSettingsCategory(page, "connection");
  await expect(dialog.getByTestId("invisible-switch")).toBeInViewport();
});

async function expectSurfaceMotionSettled(surface: Locator) {
  await expect
    .poll(async () =>
      surface.evaluate(
        (node) =>
          node
            .getAnimations({ subtree: false })
            .filter((animation) => animation.playState === "running").length,
      ),
    )
    .toBe(0);
}
