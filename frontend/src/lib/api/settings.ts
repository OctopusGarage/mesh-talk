import { invoke } from "@tauri-apps/api/core";
import {
  disable as autostartDisable,
  enable as autostartEnable,
  isEnabled as autostartIsEnabled,
} from "@tauri-apps/plugin-autostart";
import type { AppSettings } from "../types";

export const settings = {
  /** The two non-autostart toggles (minimize-to-tray, notifications). */
  get: () => invoke<AppSettings>("get_app_settings"),
  set: (value: AppSettings) =>
    invoke<void>("set_app_settings", { settings: value }),
  // Launch-at-login is owned by the autostart plugin (OS launch-agent is the
  // source of truth), so it's read/written through the plugin, not our state.
  autostartEnabled: () => autostartIsEnabled(),
  setAutostart: (on: boolean) => (on ? autostartEnable() : autostartDisable()),
};
