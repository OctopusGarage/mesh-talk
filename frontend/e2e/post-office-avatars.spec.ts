import { test, expect } from "./tauri-mock";
import { enterChat, seedTheme } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";

for (const theme of ["light", "dark"] as const) {
  test(`post office avatars in ${theme}`, async ({ page }) => {
    await seedTheme(page, theme);
    await enterChat(page, "tester", "/?data=post-office");
    await openSidebarMenuAction(page, "sidebar-nav-connection");
    await page.getByTestId("diagnostics-tab-peers").click();
    await expect(page.locator("[data-post-office-avatar]")).toHaveCount(3);
    await expect(
      page.getByRole("img", { name: "bob", exact: true }).last(),
    ).toBeVisible();
  });
}
