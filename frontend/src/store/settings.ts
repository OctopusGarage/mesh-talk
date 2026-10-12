import { create } from "zustand";
import { settings as settingsApi } from "@/lib/api";
import {
  DEFAULT_RINGTONE,
  asRingtoneId,
  type RingtoneId,
} from "@/lib/ringtones";
import type { AppSettings } from "@/lib/types";

/** App-setting flags the UI needs reactively (most settings are read in Rust, but a few —
 * like the experimental calls toggle and the ringtone — drive UI affordances / playback and
 * must re-render on change). */
interface SettingsStore {
  /** Experimental voice/video calls — off by default, opt-in via the Settings dialog. */
  callsEnabled: boolean;
  /** The chosen incoming-call ringtone. */
  ringtone: RingtoneId;
  privacy: Pick<
    AppSettings,
    | "privacy_level"
    | "notification_preview"
    | "auto_lock_seconds"
    | "lock_when_hidden"
    | "confirm_sensitive_copy"
    | "clipboard_clear_seconds"
  >;
  /** Load the persisted flags (called once at app start). */
  load: () => Promise<void>;
  /** Reflect a just-saved toggle without a round-trip. */
  setCallsEnabled: (on: boolean) => void;
  /** Reflect a just-saved ringtone choice without a round-trip. */
  setRingtone: (id: RingtoneId) => void;
  setPrivacy: (value: SettingsStore["privacy"]) => void;
}

export const balancedPrivacy: SettingsStore["privacy"] = {
  privacy_level: "balanced",
  notification_preview: true,
  auto_lock_seconds: 0,
  lock_when_hidden: false,
  confirm_sensitive_copy: false,
  clipboard_clear_seconds: 0,
};

export const useSettings = create<SettingsStore>((set) => ({
  callsEnabled: false,
  ringtone: DEFAULT_RINGTONE,
  privacy: balancedPrivacy,
  load: async () => {
    try {
      const s = await settingsApi.get();
      set({
        callsEnabled: s.calls_enabled,
        ringtone: asRingtoneId(s.ringtone),
        privacy: {
          privacy_level: s.privacy_level,
          notification_preview: s.notification_preview,
          auto_lock_seconds: s.auto_lock_seconds,
          lock_when_hidden: s.lock_when_hidden,
          confirm_sensitive_copy: s.confirm_sensitive_copy,
          clipboard_clear_seconds: s.clipboard_clear_seconds,
        },
      });
    } catch {
      // Settings unavailable (node not up yet) — keep the safe defaults.
    }
  },
  setCallsEnabled: (on) => set({ callsEnabled: on }),
  setRingtone: (id) => set({ ringtone: id }),
  setPrivacy: (value) => set({ privacy: value }),
}));
