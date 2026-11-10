import type { Page } from "@playwright/test";
import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { selectSettingsCategory, settingsCategories } from "./helpers/settings";
import { expectDialogFitsViewport } from "./helpers/ui-audit";

async function failLoadUntilRetry(page: Page, command: string) {
  await page.evaluate((blocked) => {
    const win = window as unknown as {
      __settingsBlockedLoad: string | null;
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    win.__settingsBlockedLoad = blocked;
    const original = win.__TAURI_INTERNALS__.invoke;
    win.__TAURI_INTERNALS__.invoke = (next, args) =>
      next === win.__settingsBlockedLoad
        ? Promise.reject(new Error("load unavailable"))
        : original(next, args);
  }, command);
}
async function allowLoadRetry(page: Page) {
  await page.evaluate(() => {
    (
      window as unknown as { __settingsBlockedLoad: string | null }
    ).__settingsBlockedLoad = null;
  });
}

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 760, height: 520 },
]) {
  test(`all settings categories expose one page at ${viewport.width}x${viewport.height}`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await enterChat(page);
    await openSidebarMenuAction(page, "sidebar-nav-settings");
    const dialog = page.getByTestId("settings-dialog");
    await expect(page.getByTestId("settings-page-general")).toBeVisible();
    for (const category of Object.keys(
      settingsCategories,
    ) as (keyof typeof settingsCategories)[]) {
      await selectSettingsCategory(page, category);
      const panel = page.getByTestId(`settings-page-${category}`);
      const controlByCategory = {
        general: "#setting-launch-at-login",
        shortcuts: "#setting-screenshot-capture",
        appearance: '[data-testid="theme-picker"]',
        notifications: "#setting-notifications",
        connection: "#setting-post-office",
        privacy: "#setting-privacy-level",
        integrations: "#setting-local-agent-access",
        files: '[data-testid="global-attachment-receive"]',
        history: '[data-testid="settings-retention-select"]',
      };
      await expect(panel.locator(controlByCategory[category])).toBeVisible();
      await expect(
        panel.getByRole("heading", {
          name: settingsCategories[category],
          exact: true,
        }),
      ).toBeVisible();
      await expect(
        dialog.locator('[data-testid^="settings-page-"]'),
      ).toHaveCount(1);
      await expect(dialog.locator('[aria-current="page"]')).toHaveCount(1);
      await expectDialogFitsViewport(page, "settings-dialog");
      expect(
        await dialog.evaluate(
          (element) => element.scrollWidth - element.clientWidth,
        ),
      ).toBeLessThanOrEqual(1);
      await expect(
        dialog.getByRole("heading", { name: "Settings", exact: true }),
      ).toBeInViewport();
      await expect(
        dialog.getByRole("button", { name: "Close", exact: true }),
      ).toBeInViewport();
      await expect(
        page.getByTestId(`settings-nav-${category}`),
      ).toBeInViewport();
    }
  });
}

test("retention tightening waits for confirmation and cancellation preserves history", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const win = window as unknown as {
      __retentionWrites: number[];
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    win.__retentionWrites = [];
    const original = win.__TAURI_INTERNALS__.invoke;
    win.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (
        command === "set_app_settings" &&
        typeof (args.settings as Record<string, unknown>)?.retention_days ===
          "number"
      )
        win.__retentionWrites.push(
          (args.settings as { retention_days: number }).retention_days,
        );
      return original(command, args);
    };
  });
  const writes = () =>
    page.evaluate(
      () =>
        (window as unknown as { __retentionWrites: number[] })
          .__retentionWrites,
    );
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "history");
  const retention = page.getByTestId("settings-retention-select");
  await expect(retention).toHaveValue("0");
  await retention.selectOption("30");
  await expect(
    page.getByRole("button", { name: "Delete older history", exact: true }),
  ).toBeVisible();
  expect(await writes()).toEqual([]);
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(retention).toHaveValue("0");
  expect(await writes()).toEqual([]);
  await retention.selectOption("30");
  await page
    .getByRole("button", { name: "Delete older history", exact: true })
    .click();
  await expect(retention).toHaveValue("30");
  expect(await writes()).toEqual([30]);
  await retention.selectOption("7");
  await expect(
    page.getByRole("button", { name: "Delete older history", exact: true }),
  ).toBeVisible();
  expect(await writes()).toEqual([30]);
  await page
    .getByRole("button", { name: "Create a backup first", exact: true })
    .click();
  await expect(page.getByTestId("recovery-export-key")).toBeVisible();
  expect(await writes()).toEqual([30]);
});

test("privacy preset and notification preview share saved state across pages", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "privacy");
  await page.getByLabel("Device privacy preset").selectOption("strict");
  const saveStatus = page
    .getByTestId("settings-dialog")
    .locator('[role="status"][aria-live="polite"]');
  await expect(saveStatus).toHaveText("Preset selection is not applied yet.");
  await page.getByRole("button", { name: "Apply preset", exact: true }).click();
  await expect(saveStatus).toHaveText("Saved");
  await page
    .getByRole("button", {
      name: "Manage message previews in Notifications & calls",
      exact: true,
    })
    .click();
  await expect(page.getByTestId("settings-nav-notifications")).toHaveAttribute(
    "aria-current",
    "page",
  );
  const preview = page.getByRole("switch", {
    name: "Show message previews",
    exact: true,
  });
  await expect(preview).toBeFocused();
  await expect(preview).toHaveAttribute("aria-checked", "false");
  await preview.click();
  await expect(preview).toHaveAttribute("aria-checked", "true");
  await selectSettingsCategory(page, "privacy");
  await expect(page.getByLabel("Device privacy preset")).toHaveValue("custom");
  await expect(page.getByLabel("Automatic lock")).toHaveValue("60");
  await selectSettingsCategory(page, "notifications");
  await expect(preview).toHaveAttribute("aria-checked", "true");
});

test("relay dependencies explain disabled controls and retain saved preferences", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "connection");
  const relay = page.getByRole("switch", {
    name: "Use post-office relays",
    exact: true,
  });
  const bridge = page.getByRole("switch", {
    name: "Discover peers through a post office",
    exact: true,
  });
  const send = page.getByRole("switch", {
    name: "Send through post offices",
    exact: true,
  });
  const receive = page.getByRole("switch", {
    name: "Receive through post offices",
    exact: true,
  });
  const preferred = page.locator("#setting-bridge-hub");
  await bridge.click();
  await send.click();
  await relay.click();
  await expect(bridge).toBeDisabled();
  await expect(send).toBeDisabled();
  await expect(receive).toBeDisabled();
  await expect(preferred).toBeDisabled();
  await expect(
    page
      .getByText(
        "Inactive because post-office relays are off. Your saved preference is kept.",
      )
      .first(),
  ).toBeVisible();
  await relay.click();
  await expect(bridge).toBeEnabled();
  await expect(bridge).toHaveAttribute("aria-checked", "true");
  await expect(send).toHaveAttribute("aria-checked", "false");
  await expect(receive).toHaveAttribute("aria-checked", "true");
  await page.getByTestId("invisible-switch").click();
  await page.getByTestId("invisible-confirm").click();
  await expect(bridge).toBeDisabled();
  await expect(bridge).toHaveAttribute("aria-checked", "true");
  await expect(
    page.getByText("Inactive while Invisible mode is on.").first(),
  ).toBeVisible();
});

test("unavailable privacy disables bridge discovery with an explanation", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const win = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    const original = win.__TAURI_INTERNALS__.invoke;
    win.__TAURI_INTERNALS__.invoke = (command, args) =>
      command === "get_privacy"
        ? Promise.reject(new Error("privacy unavailable"))
        : original(command, args);
  });
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "connection");
  await expect(
    page.getByRole("switch", {
      name: "Discover peers through a post office",
      exact: true,
    }),
  ).toBeDisabled();
  await expect(page.getByTestId("settings-page-connection")).toContainText(
    /privacy.*unavailable|privacy.*could.*load|privacy.*loaded/i,
  );
});

test("receiving policy load failure has an unknown disabled value and retry recovers", async ({
  page,
}) => {
  await enterChat(page);
  await failLoadUntilRetry(page, "attachment_receive_policy");
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "files");
  const settings = page.getByTestId("global-attachment-settings");
  const receive = page.getByTestId("global-attachment-receive");
  await expect(receive).toBeDisabled();
  await expect(receive).toHaveValue("");
  await expect(settings.getByRole("alert")).toBeVisible();
  await expect(settings.getByRole("alert")).not.toContainText(
    /\{\{?error\}\}?/,
  );
  await allowLoadRetry(page);
  await settings.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(receive).toBeEnabled();
  await expect(receive).toHaveValue("ask");
  await page.evaluate(() =>
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("set_attachment_global_auto"),
  );
  await receive.selectOption("auto");
  await expect(settings.getByRole("alert")).toBeVisible();
  await expect(settings.getByRole("alert")).not.toContainText(
    /\{\{?error\}\}?/,
  );
  await expect(receive).toHaveValue("ask");
  await receive.selectOption("auto");
  await expect(receive).toHaveValue("auto");
});

test("backup status read failure is distinguished from no backup and can be retried", async ({
  page,
}) => {
  await enterChat(page);
  await failLoadUntilRetry(page, "backup_status");
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "history");
  const status = page.getByTestId("backup-status-summary");
  await expect(status.getByRole("alert")).toBeVisible();
  await expect(status.getByRole("alert")).not.toContainText(/\{\{?error\}\}?/);
  await allowLoadRetry(page);
  await status.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(status.getByRole("alert")).toHaveCount(0);
  await expect(status.getByRole("status")).toBeVisible();
});

test("backup setup read failure keeps export disabled and retry preserves the key", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "history");
  await page.evaluate(() =>
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("backup_setup"),
  );
  await page
    .getByRole("button", { name: "Create encrypted backup", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("network unreachable");
  await expect(page.getByTestId("recovery-export")).toBeDisabled();
  await expect(page.getByTestId("recovery-export-key")).toHaveCount(0);
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(page.getByTestId("recovery-export-key")).toBeVisible();
  await expect(page.getByTestId("recovery-export")).toBeDisabled();
});

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 760, height: 520 },
]) {
  for (const theme of ["light", "dark"]) {
    for (const language of ["en", "es"]) {
      test(`settings layout supports ${theme} theme and ${language} locale at ${viewport.width}x${viewport.height}`, async ({
        page,
      }, testInfo) => {
        // Each case captures every category; keep the ordinary assertion deadlines.
        test.setTimeout(90_000);
        await page.setViewportSize(viewport);
        await enterChat(page);
        await openSidebarMenuAction(page, "sidebar-nav-settings");
        await selectSettingsCategory(page, "appearance");
        await page.getByTestId(`theme-${theme}`).click();
        await selectSettingsCategory(page, "appearance");
        await page
          .getByTestId("settings-language-select")
          .selectOption(language);
        for (const category of Object.keys(
          settingsCategories,
        ) as (keyof typeof settingsCategories)[]) {
          await selectSettingsCategory(page, category);
          await expectDialogFitsViewport(page, "settings-dialog");
          const dialog = page.getByTestId("settings-dialog");
          expect(
            await dialog.evaluate(
              (element) => element.scrollWidth - element.clientWidth,
            ),
          ).toBeLessThanOrEqual(1);
          await expect(
            page.getByTestId(`settings-nav-${category}`),
          ).toBeInViewport();
          await dialog.screenshot({
            path: testInfo.outputPath(
              `settings-${category}-${theme}-${language}-${viewport.width}x${viewport.height}.png`,
            ),
            animations: "disabled",
          });
        }
        await selectSettingsCategory(page, "privacy");
        await page.screenshot({
          path: testInfo.outputPath(
            `settings-${theme}-${language}-${viewport.width}x${viewport.height}.png`,
          ),
          animations: "disabled",
        });
      });
    }
  }
}

for (const viewport of [
  { width: 1280, height: 900 },
  { width: 390, height: 844 },
]) {
  test(`settings task ownership and Agent navigation at ${viewport.width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize(viewport);
    await enterChat(page);
    await openSidebarMenuAction(page, "sidebar-nav-settings");
    const dialog = page.getByTestId("settings-dialog");
    await expect(dialog.getByRole("navigation").getByRole("group")).toHaveCount(
      3,
    );
    await expect(
      page
        .getByTestId("settings-page-general")
        .locator("#setting-launch-at-login"),
    ).toBeVisible();
    await expect(dialog.locator("#setting-language")).toHaveCount(0);
    await expect(dialog.locator("#setting-screenshot-capture")).toHaveCount(0);
    await selectSettingsCategory(page, "appearance");
    await expect(dialog.locator("#setting-language")).toBeVisible();
    await selectSettingsCategory(page, "shortcuts");
    const capture = dialog.locator("#setting-screenshot-capture");
    await capture.click();
    await expect(capture).toHaveAttribute("aria-pressed", "true");
    await page.keyboard.press("Escape");
    await expect(dialog).toBeVisible();
    await expect(capture).toHaveAttribute("aria-pressed", "false");
    await dialog.screenshot({
      path: testInfo.outputPath(`shortcuts-${viewport.width}.png`),
      animations: "disabled",
    });
    await selectSettingsCategory(page, "privacy");
    await dialog.getByRole("button", { name: "Manage agent access" }).click();
    await expect(page.getByTestId("settings-nav-integrations")).toHaveAttribute(
      "aria-current",
      "page",
    );
    await expect(dialog.locator("#setting-local-agent-access")).toBeVisible();
    // No conversations are granted in this fixture, so the unavailable switch
    // cannot take focus. The destination region is the accessible fallback.
    await expect(dialog.locator("#settings-category-panel")).toBeFocused();
    await selectSettingsCategory(page, "general");
    await dialog.screenshot({
      path: testInfo.outputPath(`organized-settings-${viewport.width}.png`),
      animations: "disabled",
    });
    const bounds = (await dialog.boundingBox())!;
    expect(bounds.x).toBeGreaterThanOrEqual(0);
    expect(bounds.x + bounds.width).toBeLessThanOrEqual(viewport.width);
  });
}
