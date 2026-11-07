import type { ComponentType } from "react";
import type { SettingsCategory } from "./model";
import type { SettingsController } from "./useSettingsController";

export type SettingsPageProps = {
  controller: SettingsController;
  onDraftChange: (draft: boolean) => void;
  onAgentSettings: () => void;
  onNotifications: () => void;
};
type Page = ComponentType<SettingsPageProps>;
const loaders: Record<SettingsCategory, () => Promise<Page>> = {
  general: () => import("./GeneralSettings").then((m) => m.GeneralSettings),
  appearance: () =>
    import("./AppearanceSettings").then((m) => m.AppearanceSettings),
  shortcuts: () =>
    import("./ScreenshotShortcutSettings").then(
      (m) => m.ScreenshotShortcutSettings,
    ),
  notifications: () =>
    import("./NotificationSettings").then((m) => m.NotificationSettings),
  connection: () => import("./NetworkSettings").then((m) => m.NetworkSettings),
  privacy: () => import("./SecuritySettings").then((m) => m.SecuritySettings),
  integrations: () =>
    import("./LocalAgentSettings").then((m) => m.LocalAgentSettings),
  files: () => import("./FileSettings").then((m) => m.FileSettings),
  history: () => import("./HistorySettings").then((m) => m.HistorySettings),
};

export function loadSettingsPage(category: SettingsCategory) {
  return loaders[category]();
}
