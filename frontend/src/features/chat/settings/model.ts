/** Navigation and control destinations share one owner; stored preference keys stay stable. */
export const SETTINGS_GROUPS = [
  {
    id: "experience",
    categories: ["general", "appearance", "notifications", "shortcuts"],
  },
  { id: "privacyData", categories: ["privacy", "files", "history"] },
  { id: "connectionExtensions", categories: ["connection", "integrations"] },
] as const;
export type SettingsCategory =
  (typeof SETTINGS_GROUPS)[number]["categories"][number];
export const SETTINGS_CATEGORIES: readonly SettingsCategory[] =
  SETTINGS_GROUPS.flatMap((group) => [...group.categories]);

const CONTROL_CATEGORIES: Record<string, SettingsCategory> = {
  "setting-language": "appearance",
  "setting-wallpaper": "appearance",
  "setting-launch-at-login": "general",
  "setting-close-to-tray": "general",
  "setting-screenshot-capture": "shortcuts",
  "setting-screenshot-hidden": "shortcuts",
  "setting-screenshot-global": "shortcuts",
  "setting-notifications": "notifications",
  "setting-notification-preview": "notifications",
  "setting-calls": "notifications",
  "setting-ringtone": "notifications",
  "setting-media-device-check": "notifications",
  "setting-privacy-level": "privacy",
  "setting-auto-lock": "privacy",
  "setting-lock-when-hidden": "privacy",
  "setting-confirm-copy": "privacy",
  "setting-clipboard-clear": "privacy",
  "setting-stay-signed-in": "privacy",
  "setting-download-folder": "files",
  "setting-retention": "history",
  "setting-discovery-scope": "connection",
  "setting-bridge-discovery": "connection",
  "setting-origin-sharing": "connection",
  "setting-post-office": "connection",
  "setting-bridge-hub": "connection",
  "setting-local-agent-access": "integrations",
};
export function settingsCategoryForControl(
  controlId?: string,
): SettingsCategory | undefined {
  return controlId &&
    Object.prototype.hasOwnProperty.call(CONTROL_CATEGORIES, controlId)
    ? CONTROL_CATEGORIES[controlId]
    : undefined;
}
export type InactiveReason =
  | "loading"
  | "saving"
  | "relayOff"
  | "physicalOnly"
  | "invisible"
  | "privacyUnknown";

export function retentionTightens(previous: number, next: number): boolean {
  return (
    Number.isFinite(next) && next > 0 && (previous === 0 || next < previous)
  );
}

/** Saved preferences are retained; this projection explains effective availability. */
export function networkControlState(input: {
  loaded: boolean;
  saving: boolean;
  relayEnabled: boolean;
  physicalOnly: boolean;
  invisible: boolean | null;
  send: boolean;
  receive: boolean;
}) {
  const base: InactiveReason | null = !input.loaded
    ? "loading"
    : input.saving
      ? "saving"
      : null;
  const relayReason: InactiveReason | null =
    base ?? (!input.relayEnabled ? "relayOff" : null);
  const directoryReason: InactiveReason | null =
    relayReason ??
    (input.physicalOnly
      ? "physicalOnly"
      : input.invisible === null
        ? "privacyUnknown"
        : input.invisible
          ? "invisible"
          : null);
  return {
    relay: { disabled: relayReason !== null, reason: relayReason },
    directory: { disabled: directoryReason !== null, reason: directoryReason },
    partialReceive: input.relayEnabled && !input.send && input.receive,
  };
}
