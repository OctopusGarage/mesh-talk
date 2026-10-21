import { selectSettingsCategory } from "./helpers/settings";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { expect, test } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("reopening settings selects General", async ({ page }) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "appearance");
  await page
    .getByTestId("settings-dialog")
    .getByRole("button", { name: "Close" })
    .click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(page.getByTestId("settings-nav-general")).toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect(page.getByTestId("settings-page-general")).toBeVisible();
  await expect(page.getByTestId("settings-page-appearance")).toHaveCount(0);
});

test("post-office preference is on by default and persists across settings reopen", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "connection");
  const relay = page.getByRole("switch", { name: "Use post-office relays" });
  await expect(relay).toHaveAttribute("aria-checked", "true");
  await expect(page.getByTestId("post-office-status")).toHaveText(
    "No post office available",
  );
  await relay.click();
  await expect(relay).toHaveAttribute("aria-checked", "false");
  await expect(page.getByTestId("post-office-status")).toHaveText(
    "Post offices are turned off",
  );
  await page.getByRole("button", { name: "Close" }).click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "connection");
  await expect(relay).toHaveAttribute("aria-checked", "false");
});

test("bridge discovery is off by default and requires post-office relays", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "connection");
  const bridge = page.getByRole("switch", {
    name: "Discover peers through a post office",
  });
  const relay = page.getByRole("switch", { name: "Use post-office relays" });
  await expect(bridge).toHaveAttribute("aria-checked", "false");
  await bridge.click();
  await expect(bridge).toHaveAttribute("aria-checked", "true");
  await relay.click();
  await expect(bridge).toBeDisabled();
  await page.getByRole("button", { name: "Close" }).click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "connection");
  await expect(bridge).toHaveAttribute("aria-checked", "true");
  await expect(bridge).toBeDisabled();
});

test("selecting a directly routed post office persists across settings reopen", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=post-office");
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "connection");
  await expect(page.getByTestId("post-office-status")).toHaveText(
    "Post office available",
  );
  const preferred = page.locator("#setting-bridge-hub");
  await expect(preferred).toBeEnabled();
  await expect(preferred.locator("option")).toHaveCount(3);
  await expect(
    preferred.locator("option", { hasText: "remote relay node" }),
  ).toHaveCount(0);
  await expect(
    preferred.locator("option", { hasText: "Tailscale relay node" }),
  ).toHaveCount(1);
  await preferred.selectOption("66666666666666666666666666666666");
  await expect(preferred).toHaveValue("66666666666666666666666666666666");
  await page.getByRole("button", { name: "Close" }).click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "connection");
  await expect(preferred).toHaveValue("66666666666666666666666666666666");
});

test("a failed settings save keeps the stored value and offers recovery", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "notifications");
  const notifications = page.getByRole("switch", { name: "Notifications" });
  await expect(notifications).toBeEnabled();
  await expect(notifications).toHaveAttribute("aria-checked", "true");

  await page.evaluate(() => {
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("set_app_settings");
  });
  await notifications.click();
  await expect(page.getByTestId("settings-error")).toContainText(
    "Couldn’t save the change",
  );
  await expect(notifications).toHaveAttribute("aria-checked", "true");
  await selectSettingsCategory(page, "general");
  await selectSettingsCategory(page, "notifications");
  await expect(page.getByTestId("settings-error")).toContainText(
    "Couldn’t save the change",
  );

  await page
    .getByTestId("settings-error")
    .getByRole("button", { name: "Dismiss" })
    .click();
  await expect(page.getByTestId("settings-error")).toHaveCount(0);
  await expect(notifications).toBeEnabled();
  await notifications.click();
  await expect(notifications).toHaveAttribute("aria-checked", "false");
});

test("a failed settings load is explained and can be retried", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("get_app_settings");
  });
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "notifications");
  await expect(page.getByTestId("settings-error")).toContainText(
    "Couldn’t load settings",
  );
  const notifications = page.getByRole("switch", { name: "Notifications" });
  await expect(notifications).toHaveCount(0);
  await page
    .getByTestId("settings-error")
    .getByRole("button", { name: "Retry" })
    .click();
  await expect(page.getByTestId("settings-error")).toHaveCount(0);
  await expect(notifications).toBeEnabled();
});

test("relay directions persist independently and physical LAN scope gates bridge discovery", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "connection");
  const send = page.getByRole("switch", {
    name: "Send through post offices",
    exact: true,
  });
  const receive = page.getByRole("switch", {
    name: "Receive through post offices",
    exact: true,
  });
  const scope = page.getByRole("combobox", {
    name: "Device discovery scope",
    exact: true,
  });
  const bridge = page.getByRole("switch", {
    name: "Discover peers through a post office",
    exact: true,
  });
  await expect(send).toHaveAttribute("aria-checked", "true");
  await expect(receive).toHaveAttribute("aria-checked", "true");
  await expect(scope).toHaveValue("routed");
  await bridge.click();
  await expect(bridge).toHaveAttribute("aria-checked", "true");
  await send.click();
  await scope.selectOption("local");
  await expect(receive).toHaveAttribute("aria-checked", "true");
  await expect(bridge).toBeDisabled();
  await page.getByRole("button", { name: "Close" }).click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "connection");
  await expect(send).toHaveAttribute("aria-checked", "false");
  await expect(receive).toHaveAttribute("aria-checked", "true");
  await expect(scope).toHaveValue("local");
  await expect(bridge).toHaveAttribute("aria-checked", "true");
  await scope.selectOption("routed");
  await expect(bridge).toBeEnabled();
  await expect(bridge).toHaveAttribute("aria-checked", "true");
  await page
    .getByRole("switch", { name: "Use post-office relays", exact: true })
    .click();
  await expect(send).toBeDisabled();
  await expect(receive).toBeDisabled();
});
