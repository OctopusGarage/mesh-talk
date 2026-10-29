import type { Page } from "@playwright/test";
import { expect } from "../tauri-mock";

export const settingsCategories = {
  general: "App behavior",
  appearance: "Appearance & language",
  shortcuts: "Shortcuts",
  notifications: "Notifications & calls",
  connection: "Network & delivery",
  privacy: "Privacy & security",
  integrations: "Agent access",
  files: "Files & downloads",
  history: "History & backup",
} as const;

export type SettingsCategory = keyof typeof settingsCategories;

export async function selectSettingsCategory(
  page: Page,
  category: SettingsCategory,
) {
  const navigation = page.getByTestId(`settings-nav-${category}`);
  await navigation.click();
  await expect(navigation).toHaveAttribute("aria-current", "page");
  await expect(page.getByTestId(`settings-page-${category}`)).toBeVisible();
}
