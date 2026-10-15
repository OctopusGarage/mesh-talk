import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { expect, test } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("post-office preference is on by default and persists across settings reopen", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const relay = page.getByRole("switch", { name: "Use post-office relays" });
  await expect(relay).toHaveAttribute("aria-checked", "true");
  await relay.click();
  await expect(relay).toHaveAttribute("aria-checked", "false");
  await page.getByRole("button", { name: "Close" }).click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(relay).toHaveAttribute("aria-checked", "false");
});

test("bridge discovery is off by default and requires post-office relays", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
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
  await expect(bridge).toHaveAttribute("aria-checked", "true");
  await expect(bridge).toBeDisabled();
});

test("changing language preserves an unsaved bridge hub draft", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const hub = page.locator("#setting-bridge-hub");
  await expect(hub).toBeEnabled();
  await hub.fill("abcd");
  await page.getByTestId("settings-language-select").selectOption("ja");
  await expect(hub).toHaveValue("abcd");
});

test("a failed settings save keeps the stored value and offers recovery", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
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
  await expect(page.getByTestId("settings-error")).toContainText(
    "Couldn’t load settings",
  );
  const notifications = page.getByRole("switch", { name: "Notifications" });
  await expect(notifications).toBeDisabled();
  await page
    .getByTestId("settings-error")
    .getByRole("button", { name: "Retry" })
    .click();
  await expect(page.getByTestId("settings-error")).toHaveCount(0);
  await expect(notifications).toBeEnabled();
});
