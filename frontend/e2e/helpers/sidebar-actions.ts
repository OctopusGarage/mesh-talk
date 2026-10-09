import type { Page } from "@playwright/test";

export async function openSidebarMenuAction(
  page: Page,
  id: "sidebar-nav-connection" | "sidebar-nav-settings",
) {
  const item = page.getByTestId(id);
  if (!(await item.isVisible()))
    await page.getByTestId("sidebar-overflow").click();
  await item.click();
}
