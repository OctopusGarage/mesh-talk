import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { selectSettingsCategory } from "./helpers/settings";
import { expectDialogFitsViewport } from "./helpers/ui-audit";

for (const theme of ["light", "dark"]) {
  for (const width of [1440, 760]) {
    test(`history storage reminder ${theme} ${width}`, async ({
      page,
    }, info) => {
      await page.setViewportSize({ width, height: 800 });
      await enterChat(page);
      await page.evaluate(
        ({ theme }) => {
          document.documentElement.classList.toggle("dark", theme === "dark");
          const win = window as unknown as {
            __TAURI_INTERNALS__: {
              invoke: (command: string, args: unknown) => Promise<unknown>;
            };
          };
          const original = win.__TAURI_INTERNALS__.invoke;
          win.__TAURI_INTERNALS__.invoke = (command, args) =>
            command === "history_storage_usage"
              ? Promise.resolve({ bytes: 2 * 1024 ** 3, complete: true })
              : original(command, args);
        },
        { theme },
      );
      await openSidebarMenuAction(page, "sidebar-nav-settings");
      await selectSettingsCategory(page, "history");
      const panel = page.getByTestId("settings-page-history");
      await expect(panel.getByText("2 GiB", { exact: true })).toBeVisible();
      await expect(
        panel.getByText(/Review your history retention/),
      ).toBeVisible();
      await expect(panel.getByTestId("settings-retention-select")).toHaveValue(
        "0",
      );
      await expectDialogFitsViewport(page, "settings-dialog");
      expect(
        await panel.evaluate(
          (element) => element.scrollWidth - element.clientWidth,
        ),
      ).toBeLessThanOrEqual(1);
      await page.screenshot({
        path: info.outputPath(`history-${theme}-${width}.png`),
      });
    });
  }
}
