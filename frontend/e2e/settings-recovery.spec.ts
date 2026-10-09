import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { expect, test } from "./tauri-mock";
import { enterChat } from "./helpers/session";

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
