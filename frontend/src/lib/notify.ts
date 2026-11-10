import { create } from "zustand";
import {
  notifications,
  type NotificationPermission,
} from "./api/notifications";
import { readWithDeadline } from "./readDeadline";

interface PermissionState {
  permission: NotificationPermission | null;
  checking: boolean;
  requesting: boolean;
  refresh: () => Promise<void>;
  enable: (current?: () => boolean) => Promise<void>;
}
let observation: Promise<void> | null = null;
let authorization: Promise<void> | null = null;

/** Share in-flight native work, never cache a settled authorization as OS truth. */
export const useNotificationPermission = create<PermissionState>((set, get) => {
  const unavailable = () =>
    set({
      permission: {
        platform: get().permission?.platform ?? "other",
        can_open_settings: get().permission?.can_open_settings ?? false,
        status: "unknown",
      },
    });
  return {
    permission: null,
    checking: false,
    requesting: false,
    refresh: () => {
      if (authorization) return authorization;
      if (observation) return observation;
      set({ checking: true });
      observation = readWithDeadline(notifications.check)
        .then((permission) => {
          set({ permission });
        }, unavailable)
        .finally(() => {
          observation = null;
          set({ checking: false });
        });
      return observation;
    },
    enable: (current = () => true) => {
      if (authorization) return authorization;
      // Start the observation before assigning authorization, so refresh never
      // waits on the explicit action that is itself awaiting this observation.
      const checking = get().refresh();
      set({ requesting: true });
      authorization = checking
        .then(async () => {
          if (current() && get().permission?.status === "prompt") {
            // Native prompting has its own deadline. Do not abandon a mutation
            // through the UI read watchdog or issue another prompt while pending.
            try {
              set({ permission: await notifications.request() });
            } catch {
              unavailable();
            }
          }
        })
        .finally(() => {
          authorization = null;
          set({ requesting: false });
        });
      return authorization;
    },
  };
});

/** Refresh on return from system settings without polling or prompting. */
export function watchNotificationPermission() {
  const refresh = () => {
    void useNotificationPermission.getState().refresh();
  };
  const visible = () => {
    if (document.visibilityState === "visible") refresh();
  };
  refresh();
  window.addEventListener("focus", refresh);
  document.addEventListener("visibilitychange", visible);
  return () => {
    window.removeEventListener("focus", refresh);
    document.removeEventListener("visibilitychange", visible);
  };
}
