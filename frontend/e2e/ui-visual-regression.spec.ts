import { test, expect } from "./tauri-mock";
import {
  enterChat,
  openBobDm,
  seedThemeBeforeLoad,
} from "./helpers/session";
import {
  prepareForScreenshot,
  snapshotName,
} from "./helpers/visual-snapshot";

const THEMES = [
  "light",
  "dark",
  "oled",
  "argentina",
  "barcelona",
  "messi",
] as const;

const VIEWPORTS = [
  { width: 1280, height: 800 },
  { width: 760, height: 620 },
] as const;

test.describe.configure({ mode: "serial" });

for (const viewport of VIEWPORTS) {
  test.describe(`shell matrix ${viewport.width}x${viewport.height}`, () => {
    test.use({ viewport });

    for (const theme of THEMES) {
      test(`shell ${theme}`, async ({ page }) => {
        await seedThemeBeforeLoad(page, theme);
        await enterChat(page);
        await prepareForScreenshot(page);
        await openBobDm(page);

        const shell = page.getByTestId("chat-shell");
        await expect(shell).toBeVisible();
        await expect(shell).toHaveScreenshot(
          snapshotName("chat-shell", theme, viewport.width, viewport.height),
        );
      });
    }
  });
}
