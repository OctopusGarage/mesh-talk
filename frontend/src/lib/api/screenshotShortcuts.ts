import { invoke } from "@tauri-apps/api/core";

export interface ScreenshotRegistration {
  global_active: boolean;
  generation: number | null;
  registration_id: number;
  error: string | null;
}

export interface ScreenshotShortcutEvent {
  owner: string;
  generation: number;
  hide_window: boolean;
}

export const screenshotShortcutsApi = {
  sync: (owner: string) =>
    invoke<ScreenshotRegistration>("sync_screenshot_shortcuts", { owner }),
  wake: (owner: string) => invoke<void>("wake_screenshot_window", { owner }),
  pause: (owner: string) =>
    invoke<void>("pause_screenshot_shortcuts", { owner }),
  suspend: (owner: string, generation: number, registrationId: number) =>
    invoke<void>("suspend_screenshot_shortcuts", {
      owner,
      generation,
      registrationId,
    }),
};
