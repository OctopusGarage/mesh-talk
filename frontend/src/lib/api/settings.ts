import { invoke } from "@tauri-apps/api/core";
import {
  disable as autostartDisable,
  enable as autostartEnable,
  isEnabled as autostartIsEnabled,
} from "@tauri-apps/plugin-autostart";
import type { AppSettings } from "../types";
import { invokeSensitive } from "./sensitiveInvoke";

export interface HistoryStorageUsage {
  bytes: number;
  complete: boolean;
}

const readFor = (owner: string | null, expectedAccount: string | null) =>
  invoke<AppSettings>("get_app_settings", { owner, expectedAccount });
const writeFor = (
  owner: string | null,
  expectedAccount: string | null,
  settings: AppSettings,
) =>
  invokeSensitive<void>("set_app_settings", {
    owner,
    expectedAccount,
    settings,
  });

/** Reuse the native owner for portable preferences, live policy and retention. */
export const reconcileAccountSettings = (
  owner: string,
  expectedAccount: string,
  seedMissing = false,
) =>
  invoke<AppSettings>("reconcile_account_settings", {
    owner,
    expectedAccount,
    seedMissing,
  });

/** Build the settings IPC interface with an owner resolver supplied by the app. */
export function createSettingsApi(
  currentOwner: () => string | null,
  currentAccount: () => string | null = () => null,
  currentGeneration: () => number = () => 0,
) {
  // Settings are stored as one document. Serialize read-modify-write operations so rapid
  // changes from different controls cannot each write a stale copy of the other fields.
  let pendingUpdate: Promise<void> = Promise.resolve();
  return {
    /** Owner-scoped desktop preferences; the host normalizes compound privacy presets. */
    get: () => readFor(currentOwner(), currentAccount()),
    historyUsage: () =>
      invoke<HistoryStorageUsage>("history_storage_usage", {
        owner: currentOwner(),
        expectedAccount: currentAccount(),
      }),
    set: (value: AppSettings) =>
      writeFor(currentOwner(), currentAccount(), value),
    update: (patch: Partial<AppSettings>): Promise<void> => {
      const owner = currentOwner();
      const expectedAccount = currentAccount();
      const generation = currentGeneration();
      const requireCurrent = () => {
        if (
          currentOwner() !== owner ||
          currentAccount() !== expectedAccount ||
          currentGeneration() !== generation
        )
          throw new Error("Settings session changed");
      };
      const task = pendingUpdate.then(async () => {
        requireCurrent();
        const current = await readFor(owner, expectedAccount);
        requireCurrent();
        await invokeSensitive<void>("set_app_settings", {
          owner,
          expectedAccount,
          settings: { ...current, ...patch },
          patch,
        });
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
