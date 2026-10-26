import { test, expect } from "./helpers/portable-evidence";
import { enterChat, openBobDm, revealComposerTools } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { expectDialogFitsViewport } from "./helpers/ui-audit";

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 760, height: 520 },
]) {
  test(`custom screenshot shortcuts and hints at ${viewport.width}x${viewport.height}`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize(viewport);
    await enterChat(page);
    await openBobDm(page);
    await openSidebarMenuAction(page, "sidebar-nav-settings");
    const dialog = page.getByTestId("settings-dialog");
    await expect(page.locator("#setting-screenshot-global")).toHaveAttribute(
      "aria-checked",
      "false",
    );
    const binding = page.locator("#setting-screenshot-capture");
    await binding.click();
    const primary = await page.evaluate(() =>
      /Macintosh|Mac OS X/i.test(navigator.userAgent) ? "Meta" : "Control",
    );
    await page.keyboard.press(`${primary}+Shift+B`);
    await expect(binding).toContainText("B");
    await expect(page.getByTestId("screenshot-editor")).toHaveCount(0);
    await page.locator("#setting-screenshot-hidden").click();
    await page.keyboard.press(`${primary}+Shift+B`);
    await expect(dialog.getByRole("alert")).toContainText("different shortcut");
    await page.keyboard.press("Escape");
    await expect(dialog).toBeVisible();
    await expectDialogFitsViewport(page, "settings-dialog");
    await binding.scrollIntoViewIfNeeded();
    await page.screenshot({
      path: testInfo.outputPath("shortcut-settings.png"),
    });
    await page.keyboard.press("Escape");
    await revealComposerTools(page);
    await expect(page.getByTestId("composer-screenshot")).toHaveAttribute(
      "title",
      /B · App only/,
    );
    await page.getByTestId("composer-input").focus();
    await page.keyboard.press(`${primary}+Shift+B`);
    await expect(page.getByTestId("screenshot-editor")).toBeVisible();
    const canvas = page.getByTestId("screenshot-canvas");
    await canvas.dragTo(canvas, {
      sourcePosition: { x: 10, y: 10 },
      targetPosition: { x: 100, y: 80 },
    });
    await page.keyboard.press("r");
    await expect(
      page.getByRole("button", { name: "Rectangle", exact: true }),
    ).toHaveAttribute("aria-pressed", "true");
    await expect(
      page.getByRole("button", { name: "Rectangle", exact: true }),
    ).toHaveAttribute("title", "Rectangle · R");
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("screenshot-editor")).toHaveCount(0);
  });
}

test("global opt-in routes scoped events and exposes effective scope", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await page.locator("#setting-screenshot-global").click();
  await expect(
    page.getByText("Global shortcuts active", { exact: true }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await revealComposerTools(page);
  await expect(page.getByTestId("composer-screenshot")).toHaveAttribute(
    "title",
    /Global/,
  );
  await page.evaluate(() => {
    const emit = (
      window as unknown as {
        __mockEmit: (name: string, payload: unknown) => void;
      }
    ).__mockEmit;
    emit("screenshot-shortcut", {
      owner: "other",
      generation: 1,
      hide_window: true,
    });
    emit("screenshot-shortcut", {
      owner: "u_self",
      generation: 1,
      hide_window: true,
    });
  });
  await expect(page.getByTestId("screenshot-editor")).toBeVisible();
});
