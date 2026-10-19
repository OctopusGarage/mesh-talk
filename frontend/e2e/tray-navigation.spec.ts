import { test, expect } from "./tauri-mock";
import { waitForChatShell, enterChat } from "./helpers/session";

// Exercises the real React listeners through the Tauri event fixture. Native
// menu rendering and window activation require a separate desktop check.
for (const [action, dialog] of [
  ["settings", "settings-dialog"],
  ["diagnostics", "diagnostics-dialog"],
  ["about", "about-dialog"],
  ["help", "help-dialog"],
] as const) {
  test(`tray ${action} opens its existing dialog`, async ({ page }) => {
    await enterChat(page);
    await page.evaluate((payload) => {
      const emit = (
        window as unknown as {
          __mockEmit: (event: string, payload: unknown) => void;
        }
      ).__mockEmit;
      emit("tray-navigate", payload);
    }, action);
    await expect(page.getByTestId(dialog)).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByTestId(dialog)).not.toBeVisible();
    await waitForChatShell(page);
  });
}

test("an unknown tray destination does not open a dialog", async ({ page }) => {
  await enterChat(page);
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmit: (event: string, payload: unknown) => void;
      }
    ).__mockEmit("tray-navigate", "unknown");
  });
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await waitForChatShell(page);
});

test("switching tray destinations replaces the open dialog", async ({
  page,
}) => {
  await enterChat(page);
  for (const destination of ["settings", "help", "diagnostics", "about"]) {
    await page.evaluate((payload) => {
      (
        window as unknown as {
          __mockEmit: (event: string, payload: unknown) => void;
        }
      ).__mockEmit("tray-navigate", payload);
    }, destination);
    await expect(page.getByTestId(`${destination}-dialog`)).toBeVisible();
    await expect(page.getByRole("dialog", { includeHidden: true })).toHaveCount(
      1,
    );
  }
});

test("About remains reachable through the sidebar menu", async ({ page }) => {
  await enterChat(page);
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-about").click();
  await expect(page.getByTestId("about-dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("about-dialog")).not.toBeVisible();
});
