import type { Page } from "@playwright/test";
import { expect } from "../tauri-mock";

export const settingsCategories = {
  general: "General",
  appearance: "Appearance",
  notifications: "Notifications & calls",
  connection: "Connection & delivery",
  privacy: "Privacy & security",
  files: "Files & receiving",
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
