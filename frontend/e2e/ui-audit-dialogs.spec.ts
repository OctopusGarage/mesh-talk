import type { Locator } from "@playwright/test";
import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
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

  const cases = [
    {
      open: async () => page.getByTestId("sidebar-action-search").click(),
      surface: "search-dialog",
      hasDialogClose: true,
    },
    {
      open: async () => page.getByTestId("sidebar-action-files").click(),
      surface: "files-tray",
      hasDialogClose: false,
    },
    {
      open: async () => page.getByTestId("conversation-history-trigger").click(),
      surface: "conversation-history-dialog",
      hasDialogClose: true,
    },
    {
      open: async () => {
        await page.getByTestId("sidebar-overflow").click();
        await page.getByTestId("sidebar-action-settings").click();
      },
      surface: "settings-dialog",
      hasDialogClose: true,
    },
    {
      open: async () => page.getByTestId("open-profile").click(),
      surface: "profile-dialog",
      hasDialogClose: true,
    },
  ];

  for (const item of cases) {
    await item.open();
    const surface = page.getByTestId(item.surface);
    await expect(surface).toBeVisible();
    await expectDialogFitsViewport(page, item.surface);

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
