import { expect, type Page } from "@playwright/test";

export async function openSidebarMenuAction(
  page: Page,
  id:
    | "sidebar-nav-connection"
    | "sidebar-nav-settings"
    | "sidebar-nav-help"
    | "sidebar-lock",
) {
  const trigger = page.getByTestId("sidebar-overflow");
  if ((await trigger.getAttribute("aria-expanded")) !== "true")
    await trigger.click();
  await expect(trigger).toHaveAttribute("aria-expanded", "true");
  const menu = page.getByTestId("sidebar-overflow-menu");
  await expect(menu).toHaveAttribute("data-state", "open");
  await menu.getByTestId(id).click();
}
