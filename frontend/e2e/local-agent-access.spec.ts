import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { expectDialogFitsViewport } from "./helpers/ui-audit";

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 760, height: 520 },
]) {
  test(`local agent settings authorize, disclose and copy at ${viewport.width}x${viewport.height}`, async ({
    page,
    context,
  }, testInfo) => {
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
    await page.setViewportSize(viewport);
    await enterChat(page);
    await openSidebarMenuAction(page, "sidebar-nav-settings");
    const panel = page.getByTestId("local-agent-settings");
    const toggle = panel.locator("#setting-local-agent-access");
    await expect(toggle).toBeDisabled();
    await panel.getByRole("checkbox", { name: /bob/i }).check();
    await expect(toggle).toBeEnabled();
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "true");
    await expect(panel.getByRole("status")).toContainText("Running");
    await panel.locator("summary").click();
    const instructions = await panel.locator("pre").innerText();
    expect(instructions).toContain("/v1/help");
    expect(instructions).not.toContain("agent-access.json");
    await panel.getByTestId("copy-agent-instructions").click();
    await expect
      .poll(() => page.evaluate(() => navigator.clipboard.readText()))
      .toBe(instructions);
    await expectDialogFitsViewport(page, "settings-dialog");
    await page.screenshot({
      path: testInfo.outputPath("local-agent-settings.png"),
    });
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "false");
    await expect(panel.locator("pre")).toHaveCount(0);
    await toggle.click();
    await expect(panel.locator("summary")).toHaveCount(1);
  });
}
