import { invoke } from "@tauri-apps/api/core";

export interface NotificationPermission {
  status: "granted" | "denied" | "prompt" | "limited" | "unknown";
  platform: "macos" | "windows" | "linux" | "other";
  can_open_settings: boolean;
}

/** Machine-wide OS observations, independent of the account's saved preference. */
export const notifications = {
  check: () => invoke<NotificationPermission>("notification_permission"),
  request: () =>
    invoke<NotificationPermission>("request_notification_permission"),
  openSettings: () => invoke<void>("open_notification_settings"),
};
