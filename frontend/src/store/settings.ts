import { create } from "zustand";
import { settings as settingsApi } from "@/lib/api";
import {
  DEFAULT_RINGTONE,
  asRingtoneId,
  type RingtoneId,
} from "@/lib/ringtones";
import {
  balancedPrivacy,
  privacyFromSettings,
  type PrivacySettings,
} from "@/lib/privacySettings";
import { captureRuntimeOwner } from "./ownership";

/** App-setting flags the UI needs reactively (most settings are read in Rust, but a few —
 * like the experimental calls toggle and the ringtone — drive UI affordances / playback and
 * must re-render on change). */
interface SettingsStore {
  owner: string | null;
  setOwner: (owner: string | null) => void;
  /** Experimental voice/video calls — off by default, opt-in via the Settings dialog. */
  callsEnabled: boolean;
  notificationsEnabled: boolean | null;
  setNotificationsEnabled: (on: boolean) => void;
  /** The chosen incoming-call ringtone. */
  ringtone: RingtoneId;
  privacy: PrivacySettings;
  /** Load the persisted flags (called once at app start). */
  load: () => Promise<void>;
  /** Reflect a just-saved toggle without a round-trip. */
  setCallsEnabled: (on: boolean) => void;
  /** Reflect a just-saved ringtone choice without a round-trip. */
  setRingtone: (id: RingtoneId) => void;
  setPrivacy: (value: SettingsStore["privacy"]) => void;
}

// Preserve the existing facade for settings consumers.
export { balancedPrivacy } from "@/lib/privacySettings";

export const useSettings = create<SettingsStore>((set, get) => ({
  owner: null,
  setOwner: (owner) => {
    if (get().owner === owner) return;
    set({
      owner,
      callsEnabled: false,
      notificationsEnabled: null,
      ringtone: DEFAULT_RINGTONE,
      privacy: balancedPrivacy,
    });
  },
  callsEnabled: false,
  notificationsEnabled: null,
  ringtone: DEFAULT_RINGTONE,
  privacy: balancedPrivacy,
  load: async () => {
    const lease = captureRuntimeOwner();
    if (!lease.owner || !lease.current() || get().owner !== lease.owner) return;
    try {
      const s = await settingsApi.get();
      if (!lease.current() || get().owner !== lease.owner) return;
      set({
        callsEnabled: s.calls_enabled,
        notificationsEnabled: s.notifications,
        ringtone: asRingtoneId(s.ringtone),
        privacy: privacyFromSettings(s),
      });
    } catch {
      // Settings unavailable (node not up yet) — keep the safe defaults.
    }
  },
  setNotificationsEnabled: (on) => set({ notificationsEnabled: on }),
  setCallsEnabled: (on) => set({ callsEnabled: on }),
  setRingtone: (id) => set({ ringtone: id }),
  setPrivacy: (value) => set({ privacy: value }),
}));
