import type { AppSettings } from "./types";

/** Device-local presentation privacy; network admission has its own protocol owner. */
export type PrivacySettings = Pick<
  AppSettings,
  | "privacy_level"
  | "notification_preview"
  | "auto_lock_seconds"
  | "lock_when_hidden"
  | "confirm_sensitive_copy"
  | "clipboard_clear_seconds"
>;

export const balancedPrivacy: PrivacySettings = {
  privacy_level: "balanced",
  notification_preview: true,
  auto_lock_seconds: 0,
  lock_when_hidden: false,
  confirm_sensitive_copy: false,
  clipboard_clear_seconds: 0,
};

/** Shared by initial store loading and settings-dialog reload/save settlement. */
export function privacyFromSettings(
  settings: PrivacySettings,
): PrivacySettings {
  return {
    privacy_level: settings.privacy_level,
    notification_preview: settings.notification_preview,
    auto_lock_seconds: settings.auto_lock_seconds,
    lock_when_hidden: settings.lock_when_hidden,
    confirm_sensitive_copy: settings.confirm_sensitive_copy,
    clipboard_clear_seconds: settings.clipboard_clear_seconds,
  };
}
