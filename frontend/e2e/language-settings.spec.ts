import { readFile } from "node:fs/promises";
import { expect, test } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { selectSettingsCategory, settingsCategories } from "./helpers/settings";

for (const language of ["pt-BR", "ko"]) {
  for (const viewport of [
    { width: 1440, height: 1000 },
    { width: 760, height: 520 },
  ]) {
    test(`${language} settings switch, fit and persist at ${viewport.width}px`, async ({
      page,
    }, testInfo) => {
      await page.setViewportSize(viewport);
      const messages = JSON.parse(
        await readFile(
          new URL(`../src/locales/${language}.json`, import.meta.url),
          "utf8",
        ),
      );
      await enterChat(page);
      await openSidebarMenuAction(page, "sidebar-nav-settings");
      await selectSettingsCategory(page, "appearance");
      const selector = page.getByTestId("settings-language-select");
      if (language === "pt-BR")
        await expect(selector.locator('option[value="pt-BR"]')).toHaveText(
          "Português",
        );
      await selector.selectOption(language);
      await expect(page.locator("html")).toHaveAttribute("lang", language);
      await expect(selector).toHaveValue(language);
      await expect(page.getByTestId("settings-page-appearance")).toContainText(
        messages.settings.language,
      );
      for (const category of Object.keys(
        settingsCategories,
      ) as (keyof typeof settingsCategories)[]) {
        await selectSettingsCategory(page, category);
        await expect(page.getByTestId(`settings-nav-${category}`)).toHaveText(
          messages.settings.categories[category],
        );
        const dialog = page.getByTestId("settings-dialog");
        expect(
          await dialog.evaluate(
            (element) => element.scrollWidth - element.clientWidth,
          ),
        ).toBeLessThanOrEqual(1);
        await expect(
          page.getByTestId(`settings-nav-${category}`),
        ).toBeInViewport();
      }
      await selectSettingsCategory(page, "appearance");
      await page.getByTestId("settings-dialog").screenshot({
        path: testInfo.outputPath(`${language}-${viewport.width}.png`),
        animations: "disabled",
      });
      expect(
        await page.evaluate(() => localStorage.getItem("mesh-talk-lang")),
      ).toBe(language);
      await page.reload();
      await expect(page.locator("html")).toHaveAttribute("lang", language);
      await expect(page.getByTestId("login-submit")).toBeVisible();
      await enterChat(page);
      await openSidebarMenuAction(page, "sidebar-nav-settings");
      await selectSettingsCategory(page, "appearance");
      await expect(page.getByTestId("settings-language-select")).toHaveValue(
        language,
      );
      await page.getByTestId("settings-language-select").selectOption("en");
      await expect(page.locator("html")).toHaveAttribute("lang", "en");
    });
  }
}

for (const [browserLanguage, expected] of [
  ["pt-PT", "pt-BR"],
  ["ko-KR", "ko"],
]) {
  test(`${browserLanguage} browser preference selects ${expected} on first run`, async ({
    page,
  }) => {
    await page.addInitScript((tag) => {
      Object.defineProperty(navigator, "languages", {
        value: ["unsupported", tag],
        configurable: true,
      });
    }, browserLanguage);
    await page.goto("/");
    await expect(page.locator("html")).toHaveAttribute("lang", expected);
    await expect(page.getByTestId("login-submit")).toBeVisible();
  });
}
