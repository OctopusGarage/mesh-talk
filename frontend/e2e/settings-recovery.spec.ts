import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { expect, test } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("reopening settings selects the visible privacy section", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const dialog = page.getByTestId("settings-dialog");
  const privacy = dialog.getByRole("button", {
    name: "Network privacy",
    exact: true,
  });
  const appearance = dialog.getByRole("button", {
    name: "Appearance",
    exact: true,
  });
  await appearance.click();
  await expect(appearance).toHaveAttribute("aria-current", "location");
  await dialog.getByRole("button", { name: "Close" }).click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(privacy).toHaveAttribute("aria-current", "location");
  await expect(appearance).not.toHaveAttribute("aria-current", "location");
});

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

test("selecting a preferred post office persists across settings reopen", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=post-office");
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const preferred = page.locator("#setting-bridge-hub");
  await expect(preferred).toBeEnabled();
  await expect(preferred.locator("option")).toHaveCount(2);
  await expect(
    preferred.locator("option", { hasText: "remote relay node" }),
  ).toHaveCount(0);
  await preferred.selectOption("44444444444444444444444444444444");
  await expect(preferred).toHaveValue("44444444444444444444444444444444");
  await page.getByRole("button", { name: "Close" }).click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(preferred).toHaveValue("44444444444444444444444444444444");
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

test("relay directions persist independently and physical LAN scope gates bridge discovery", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const send = page.getByRole("switch", {
    name: "Send through post offices",
    exact: true,
  });
  const receive = page.getByRole("switch", {
    name: "Receive through post offices",
    exact: true,
  });
  const scope = page.getByRole("switch", {
    name: "Physical LAN discovery only",
    exact: true,
  });
  const bridge = page.getByRole("switch", {
    name: "Discover peers through a post office",
    exact: true,
  });
  await expect(send).toHaveAttribute("aria-checked", "true");
  await expect(receive).toHaveAttribute("aria-checked", "true");
  await send.click();
  await scope.click();
  await expect(receive).toHaveAttribute("aria-checked", "true");
  await expect(bridge).toBeDisabled();
  await page.getByRole("button", { name: "Close" }).click();
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await expect(send).toHaveAttribute("aria-checked", "false");
  await expect(receive).toHaveAttribute("aria-checked", "true");
  await expect(scope).toHaveAttribute("aria-checked", "true");
  await page
    .getByRole("switch", { name: "Use post-office relays", exact: true })
    .click();
  await expect(send).toBeDisabled();
  await expect(receive).toBeDisabled();
});
