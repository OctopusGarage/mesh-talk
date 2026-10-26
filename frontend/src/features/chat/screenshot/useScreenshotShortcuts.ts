import { useEffect, useLayoutEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { useAuth } from "@/store/auth";
import { useSettings } from "@/store/settings";
import { useChat } from "@/store/chat";
import { useAppLock } from "@/store/appLock";
import { captureRuntimeOwner } from "@/store/ownership";
import { errorMessage } from "@/lib/error";
import {
  screenshotShortcutsApi,
  type ScreenshotShortcutEvent,
  type ScreenshotRegistration,
} from "@/lib/api/screenshotShortcuts";
import { installScreenshotShortcutRouter } from "./shortcutRouter";

export function useScreenshotShortcuts(options: {
  available: boolean;
  blocked: boolean;
  writable: boolean;
  capture: (hideWindow: boolean) => Promise<void>;
}) {
  const { t } = useTranslation();
  const latest = useRef(options);
  useLayoutEffect(() => {
    latest.current = options;
  });
  const owner = useAuth((s) => s.user?.id ?? null);
  const generation = useAuth((s) => s.generation);
  const ready = useChat((s) => s.ready);
  const runEpoch = useChat((s) => s.runEpoch);
  const identityEpoch = useChat((s) => s.identityEpoch);
  const bindings = useSettings((s) => s.screenshotShortcuts);
  const loaded = useSettings((s) => s.screenshotShortcutsLoaded);
  const recording = useSettings((s) => s.screenshotRecording);

  useEffect(() => {
    if (!owner || !ready || !loaded || recording) return;
    const lease = captureRuntimeOwner();
    let current = true;
    let registration: ScreenshotRegistration | null = null;
    const suspend = (status: ScreenshotRegistration | null) => {
      if (status?.generation != null)
        void screenshotShortcutsApi
          .suspend(owner, status.generation, status.registration_id)
          .catch(() => {});
    };
    const valid = () =>
      current && lease.current() && !useAppLock.getState().locked;
    const report = (error: unknown) => {
      if (valid()) useChat.getState().setError(errorMessage(error));
    };
    const release = installScreenshotShortcutRouter({
      read: () => {
        const store = useSettings.getState();
        return {
          current: valid(),
          ...latest.current,
          owner,
          generation: store.screenshotRegistration.generation,
          globalActive: store.screenshotRegistration.global_active,
          bindings: store.screenshotShortcuts,
        };
      },
      capture: (hidden) => latest.current.capture(hidden),
      wake: async () => {
        if (!valid()) return;
        await screenshotShortcutsApi.wake(owner);
        if (valid())
          useChat
            .getState()
            .setError(
              t(
                latest.current.available
                  ? "screenshot.shortcuts.chooseConversation"
                  : "screenshot.unavailable",
              ),
            );
      },
      listen: (callback) =>
        listen<ScreenshotShortcutEvent>("screenshot-shortcut", ({ payload }) =>
          callback(payload),
        ),
      report,
    });
    void release.ready
      .then(async (listening) => {
        if (!valid()) return;
        if (!listening) {
          const store = useSettings.getState();
          suspend(store.screenshotRegistration);
          useSettings.setState({
            screenshotRegistration: {
              ...store.screenshotRegistration,
              global_active: false,
            },
          });
          return;
        }
        registration = await useSettings.getState().syncScreenshotShortcuts();
        if (!valid()) suspend(registration);
      })
      .catch(report);
    return () => {
      current = false;
      release();
      // Retry may have replaced this view's initial registration. Snapshot its latest token
      // before another mount can synchronize; native ignores stale cleanup tokens.
      const status = useSettings.getState().screenshotRegistration;
      suspend(
        status.generation === registration?.generation ? status : registration,
      );
    };
  }, [
    owner,
    generation,
    ready,
    runEpoch,
    identityEpoch,
    loaded,
    recording,
    bindings,
    t,
  ]);
}
