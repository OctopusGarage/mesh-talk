import { expect, test } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import {
  expectDialogFitsViewport,
  expectElementsWithin,
  expectNoHorizontalOverflow,
} from "./helpers/ui-audit";

test.use({ viewport: { width: 760, height: 520 } });

test("first run keeps the form reachable at the minimum window size", async ({
  page,
}) => {
  await page.goto("/");
  await expect(page.getByTestId("login-form")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="login-submit"]', "body");
  await page.getByTestId("login-tab-register").click();
  await expectElementsWithin(page, '[data-testid="login-submit"]', "body");
  await expectNoHorizontalOverflow(page, "first run");
});

test("conversation and destinations remain usable at the minimum size", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await expectElementsWithin(page, '[data-testid="composer-input"]', "body");
  await expectElementsWithin(page, '[data-testid="sidebar-overflow"]', "body");
  await expectNoHorizontalOverflow(page, "chat shell");

  for (const [trigger, surface] of [
    ["sidebar-action-files", "files-tray"],
    ["sidebar-nav-connection", "diagnostics-dialog"],
    ["sidebar-nav-settings", "settings-dialog"],
  ]) {
    if (trigger === "sidebar-action-files")
      await page.getByTestId(trigger).click();
    else
      await openSidebarMenuAction(
        page,
        trigger as "sidebar-nav-connection" | "sidebar-nav-settings",
      );
    await expect(page.getByTestId(surface)).toBeVisible();
    await expectDialogFitsViewport(page, surface);
    await page.keyboard.press("Escape");
  }
});

test("conversation controls stay inside a narrow pane after sidebar resize", async ({
  page,
}) => {
  await page.addInitScript(() => {
    localStorage.setItem("mesh-talk-sidebar-width", "460");
  });
  await enterChat(page);
  await openBobDm(page);
  await expectElementsWithin(
    page,
    '[data-testid="conversation-history-trigger"], [data-testid="verify-trigger"]',
    '[data-testid="conversation-header"]',
  );
  await expectElementsWithin(page, '[data-testid="composer-input"]', "main");
  await expectNoHorizontalOverflow(page, "narrow conversation pane");
});
