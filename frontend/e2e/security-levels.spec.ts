import { expect, test } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";

test("privacy presets remain editable and manual lock requires reauthentication", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await page.getByLabel("Privacy level").selectOption("strict");
  await expect(page.getByLabel("Automatic lock")).toHaveValue("60");
  await expect(
    page.getByRole("switch", { name: "Lock when hidden" }),
  ).toHaveAttribute("aria-checked", "true");
  await page.getByRole("switch", { name: "Lock when hidden" }).click();
  await expect(page.getByLabel("Privacy level")).toHaveValue("custom");
  await page.getByRole("dialog").press("Escape");

  await openSidebarMenuAction(page, "sidebar-lock");
  await expect(page.getByTestId("lock-screen")).toBeVisible();
  await expect(page.getByTestId("chat-shell")).toHaveCount(0);
  await page.getByPlaceholder("Password").fill("wrong");
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByTestId("lock-screen")).toBeVisible();
  await page.getByPlaceholder("Password").fill("password123");
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByTestId("chat-shell")).toBeVisible();
});
