import type { Page } from "@playwright/test";
import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, seedThemeBeforeLoad } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { selectSettingsCategory } from "./helpers/settings";
import { expectDialogFitsViewport } from "./helpers/ui-audit";

type Status = "granted" | "denied" | "prompt" | "unknown" | "limited";
async function permission(page: Page, status: Status, failOpen = false) {
  await page.evaluate(
    ({ status, failOpen }) => {
      const win = window as unknown as {
        __permission: {
          status: string;
          requests: number;
          opens: number;
          failOpen: boolean;
          hold: boolean;
          release?: () => void;
        };
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      };
      win.__permission = {
        status,
        failOpen,
        requests: 0,
        opens: 0,
        hold: false,
      };
      const original = win.__TAURI_INTERNALS__.invoke;
      win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
        const state = win.__permission;
        if (
          cmd === "notification_permission" ||
          cmd === "request_notification_permission"
        ) {
          if (cmd === "request_notification_permission") {
            state.requests++;
            state.status = "denied";
          }
          if (state.hold)
            await new Promise<void>((resolve) => {
              state.release = resolve;
            });
          return {
            status: state.status,
            platform: "macos",
            can_open_settings: true,
          };
        }
        if (cmd === "open_notification_settings") {
          state.opens++;
          if (state.failOpen) throw new Error("settings unavailable");
          return;
        }
        return original(cmd, args);
      };
    },
    { status, failOpen },
  );
}
async function settings(page: Page) {
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "notifications");
}
async function status(page: Page, next: Status) {
  await page.evaluate((next) => {
    (
      window as unknown as { __permission: { status: string } }
    ).__permission.status = next;
    window.dispatchEvent(new Event("focus"));
  }, next);
}
async function send(page: Page, text: string) {
  await page.getByTestId("composer-input").fill(text);
  await page.getByTestId("composer-send").click();
  await expect(
    page.getByTestId("message-bubble").filter({ hasText: text }),
  ).toBeVisible();
  await expect(page.getByTestId("composer-input")).toHaveValue("");
}

for (const viewport of [
  { width: 1280, height: 800, theme: "light" },
  { width: 760, height: 520, theme: "dark" },
]) {
  test(`blocked notification guidance fits ${viewport.width}x${viewport.height} ${viewport.theme}`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize(viewport);
    await seedThemeBeforeLoad(page, viewport.theme);
    await enterChat(page);
    await permission(page, "denied");
    await settings(page);
    const help = page.getByTestId("notification-permission");
    await expect(help).toContainText(
      "Notifications are blocked in system settings",
    );
    await expect(page.locator("#setting-notifications")).toBeChecked();
    await expectDialogFitsViewport(page, "settings-dialog");
    const toggleBounds = await page
      .locator("#setting-notifications")
      .boundingBox();
    const titleBounds = await page
      .locator('label[for="setting-notifications"]')
      .boundingBox();
    expect(Math.abs(toggleBounds!.y - titleBounds!.y)).toBeLessThan(6);
    expect(
      await help.evaluate(
        (element) => element.scrollWidth - element.clientWidth,
      ),
    ).toBeLessThanOrEqual(1);
    const open = help.getByRole("button", { name: "Open system settings" });
    await open.focus();
    await expect(open).toBeFocused();
    await testInfo.attach(`notification-settings-${viewport.theme}`, {
      body: await page.screenshot(),
      contentType: "image/png",
    });
    await status(page, "granted");
    await expect(help).toContainText("Allowed by your system");
    await expect(
      help.getByRole("button", { name: "Open system settings" }),
    ).toHaveCount(0);
  });
}

test("explicit enable requests permission once and denial preserves the saved preference", async ({
  page,
}) => {
  await enterChat(page);
  await permission(page, "prompt");
  await settings(page);
  const toggle = page.locator("#setting-notifications");
  await toggle.click();
  await expect(toggle).not.toBeChecked();
  await expect(page.getByTestId("notification-permission")).toHaveCount(0);
  await toggle.click();
  await expect(toggle).toBeChecked();
  await expect(page.getByTestId("notification-permission")).toContainText(
    "Notifications are blocked",
  );
  const counts = await page.evaluate(
    () =>
      (
        window as unknown as {
          __permission: { requests: number; opens: number };
        }
      ).__permission,
  );
  expect(counts.requests).toBe(1);
  expect(counts.opens).toBe(0);
});

test("failed system-settings navigation preserves manual instructions and retry", async ({
  page,
}) => {
  await enterChat(page);
  await permission(page, "denied", true);
  await settings(page);
  const help = page.getByTestId("notification-permission");
  await help.getByRole("button", { name: "Open system settings" }).click();
  await expect(help.getByRole("alert")).toContainText(
    "Couldn’t open system settings",
  );
  await expect(help).toContainText(
    "System Settings → Notifications → Mesh-Talk",
  );
  await status(page, "granted");
  await expect(help).toContainText("Allowed by your system");
});

test("blocked notifications never delay sends and a dismissed reminder stays dismissed", async ({
  page,
}) => {
  await enterChat(page);
  await permission(page, "denied");
  await openBobDm(page);
  await send(page, "Sent with notifications blocked");
  const notice = page.getByTestId("notification-permission-notice");
  await expect(notice).toContainText("Messages still send");
  await notice
    .getByRole("button", { name: "Dismiss notification reminder" })
    .click();
  await send(page, "Another message without another reminder");
  await expect(notice).toHaveCount(0);
  await status(page, "denied");
  await expect(notice).toHaveCount(0);
});

test("a pending permission check does not hold the composer or outgoing message", async ({
  page,
}) => {
  await enterChat(page);
  await permission(page, "denied");
  await openBobDm(page);
  await page.evaluate(() => {
    (
      window as unknown as { __permission: { hold: boolean } }
    ).__permission.hold = true;
  });
  await send(page, "Permission query is still pending");
  await expect(page.getByTestId("composer-input")).toBeEnabled();
  await page.evaluate(() => {
    const state = (
      window as unknown as {
        __permission: { hold: boolean; release?: () => void };
      }
    ).__permission;
    state.hold = false;
    state.release?.();
  });
  await expect(
    page.getByTestId("notification-permission-notice"),
  ).toBeVisible();
});

test("disabled notifications do not show messaging guidance", async ({
  page,
}) => {
  await enterChat(page);
  await permission(page, "denied");
  await settings(page);
  await page.locator("#setting-notifications").click();
  await expect(page.locator("#setting-notifications")).not.toBeChecked();
  await page
    .getByTestId("settings-dialog")
    .getByRole("button", { name: "Close", exact: true })
    .click();
  await openBobDm(page);
  await send(page, "Notifications intentionally disabled");
  await expect(page.getByTestId("notification-permission-notice")).toHaveCount(
    0,
  );
});
