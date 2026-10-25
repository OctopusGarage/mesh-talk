import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { settings as settingsApi } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { privacyFromSettings } from "@/lib/privacySettings";
import { asRingtoneId } from "@/lib/ringtones";
import type { AppSettings } from "@/lib/types";
import { useAuth } from "@/store/auth";
import { useSettings } from "@/store/settings";
import { createSettingsDialogSession } from "../settingsDialogSession";
import type { SettingsCategory } from "./model";

interface SettingError {
  kind: "load" | "save" | "refresh";
  detail: string;
}
class SavedRefreshError extends Error {}
class AutostartRefreshError extends Error {}

/** All pages share one acknowledged snapshot and the existing ordered save seam. */
export function useSettingsController(open: boolean) {
  const owner = useAuth((s) => s.user?.id ?? null);
  const generation = useAuth((s) => s.generation);
  const ownerKey = owner === null ? null : `${owner}:${generation}`;
  const [values, setValues] = useState<AppSettings | null>(null);
  const [loadedOwner, setLoadedOwner] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [savedCategory, setSavedCategory] = useState<SettingsCategory | null>(
    null,
  );
  const [loadError, setLoadError] = useState<SettingError | null>(null);
  const [errors, setErrors] = useState<
    Partial<Record<SettingsCategory, SettingError>>
  >({});
  const [autostart, setAutostart] = useState<{ on: boolean; loaded: boolean }>({
    on: false,
    loaded: false,
  });
  const session = useRef<ReturnType<typeof createSettingsDialogSession> | null>(
    null,
  );
  if (!session.current)
    session.current = createSettingsDialogSession(setSaving);
  useLayoutEffect(() => {
    session.current!.setOwner(ownerKey);
  }, [ownerKey]);

  const install = useCallback((settings: AppSettings) => {
    const next = {
      ...settings,
      post_office_send_enabled: settings.post_office_send_enabled ?? true,
      post_office_receive_enabled: settings.post_office_receive_enabled ?? true,
      physical_lan_only: settings.physical_lan_only ?? false,
      bridge_discovery_enabled: settings.bridge_discovery_enabled ?? false,
      origin_sharing_enabled: settings.origin_sharing_enabled ?? false,
      bridge_hub_id: settings.bridge_hub_id ?? "",
    };
    setValues(next);
    const store = useSettings.getState();
    store.setPrivacy(privacyFromSettings(next));
    store.setCallsEnabled(next.calls_enabled);
    store.setNotificationsEnabled(next.notifications);
    store.setRingtone(asRingtoneId(next.ringtone));
  }, []);

  const reload = useCallback(() => {
    const request = session.current!.beginLoad();
    setValues(null);
    setLoadedOwner(null);
    setLoadError(null);
    setErrors({});
    setSavedCategory(null);
    setAutostart({ on: false, loaded: false });
    void session.current!.load(
      request,
      () => settingsApi.get(),
      (settings) => {
        install(settings);
        setLoadedOwner(ownerKey);
      },
      (error) => setLoadError({ kind: "load", detail: errorMessage(error) }),
    );
    void session.current!.load(
      request,
      () => settingsApi.autostartEnabled(),
      (on) => {
        setAutostart({ on, loaded: true });
      },
      (error) =>
        setErrors((previous) => ({
          ...previous,
          general: { kind: "load", detail: errorMessage(error) },
        })),
    );
  }, [ownerKey, install]);

  useEffect(() => {
    if (open) reload();
    const current = session.current!;
    return () => current.invalidateView();
  }, [open, reload]);

  const settings = open && loadedOwner === ownerKey ? values : null;
  const clearError = (category: SettingsCategory) =>
    setErrors((previous) => ({ ...previous, [category]: undefined }));

  const save = async <T>(
    category: SettingsCategory,
    write: () => Promise<T>,
    apply: (value: T) => void,
  ) => {
    if (!settings) return false;
    clearError(category);
    setSavedCategory(null);
    let accepted = false;
    await session.current!.save(
      write,
      (value) => {
        apply(value);
        accepted = true;
        setSavedCategory(category);
      },
      (error) => {
        const failed: SettingError = {
          kind:
            error instanceof SavedRefreshError
              ? "refresh"
              : error instanceof AutostartRefreshError
                ? "load"
                : "save",
          detail: errorMessage(error),
        };
        setErrors((previous) => ({ ...previous, [category]: failed }));
        if (failed.kind === "refresh") {
          setValues(null);
          setLoadError(failed);
        }
        if (error instanceof AutostartRefreshError)
          setAutostart((previous) => ({ ...previous, loaded: false }));
      },
    );
    return accepted;
  };

  const update = (patch: Partial<AppSettings>, category: SettingsCategory) =>
    save(
      category,
      async () => {
        await settingsApi.update(patch);
        // Presets are normalized by the backend; never duplicate that policy in each page.
        try {
          return await settingsApi.get();
        } catch (error) {
          throw new SavedRefreshError(errorMessage(error));
        }
      },
      install,
    );

  const setAutostartEnabled = (on: boolean) => {
    if (!autostart.loaded) return Promise.resolve(false);
    return save(
      "general",
      async () => {
        await settingsApi.setAutostart(on);
        try {
          return await settingsApi.autostartEnabled();
        } catch (error) {
          throw new AutostartRefreshError(errorMessage(error));
        }
      },
      (enabled) => setAutostart({ on: enabled, loaded: true }),
    );
  };

  const chooseDownloadDir = async () => {
    if (!settings) return;
    const request = session.current!.request();
    try {
      const directory = await openDialog({
        directory: true,
        defaultPath: settings.download_dir || undefined,
      });
      if (session.current!.isCurrent(request) && typeof directory === "string")
        await update({ download_dir: directory }, "files");
    } catch (error) {
      if (session.current!.isCurrent(request))
        setErrors((previous) => ({
          ...previous,
          files: { kind: "save", detail: errorMessage(error) },
        }));
    }
  };

  return {
    owner,
    identity: ownerKey,
    settings,
    saving,
    savedCategory,
    loadError,
    errors,
    autostart,
    reload,
    clearError,
    update,
    setAutostartEnabled,
    chooseDownloadDir,
  };
}

export type SettingsController = ReturnType<typeof useSettingsController>;
