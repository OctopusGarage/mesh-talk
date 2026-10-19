import { invoke } from "@tauri-apps/api/core";
import {
  disable as autostartDisable,
  enable as autostartEnable,
  isEnabled as autostartIsEnabled,
} from "@tauri-apps/plugin-autostart";
import type { AppSettings } from "../types";

const readFor = (owner: string | null) =>
  invoke<AppSettings>("get_app_settings", { owner });
const writeFor = (owner: string | null, settings: AppSettings) =>
  invoke<void>("set_app_settings", { owner, settings });

/** Build the settings IPC interface with an owner resolver supplied by the app. */
export function createSettingsApi(currentOwner: () => string | null) {
  // Settings are stored as one document. Serialize read-modify-write operations so rapid
  // changes from different controls cannot each write a stale copy of the other fields.
  let pendingUpdate: Promise<void> = Promise.resolve();
  return {
    /** Owner-scoped desktop preferences; the host normalizes compound privacy presets. */
    get: () => readFor(currentOwner()),
    set: (value: AppSettings) => writeFor(currentOwner(), value),
    update: (patch: Partial<AppSettings>): Promise<void> => {
      const owner = currentOwner();
      const task = pendingUpdate.then(async () => {
        const current = await readFor(owner);
        await writeFor(owner, { ...current, ...patch });
      });
      pendingUpdate = task.catch(() => {});
      return task;
    },
    // Launch-at-login is owned by the autostart plugin (OS launch-agent is the
    // source of truth), so it's read/written through the plugin, not our state.
    autostartEnabled: () => autostartIsEnabled(),
    setAutostart: (on: boolean) =>
      on ? autostartEnable() : autostartDisable(),
  };
}
