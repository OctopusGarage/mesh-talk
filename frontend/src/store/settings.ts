import { create } from "zustand";
import { settings as settingsApi } from "@/lib/api";
import {
  DEFAULT_RINGTONE,
  asRingtoneId,
  type RingtoneId,
} from "@/lib/ringtones";

/** App-setting flags the UI needs reactively (most settings are read in Rust, but a few —
 * like the experimental calls toggle and the ringtone — drive UI affordances / playback and
 * must re-render on change). */
interface SettingsStore {
  /** Experimental voice/video calls — off by default, opt-in via the Settings dialog. */
  callsEnabled: boolean;
  /** The chosen incoming-call ringtone. */
  ringtone: RingtoneId;
  /** Load the persisted flags (called once at app start). */
  load: () => Promise<void>;
  /** Reflect a just-saved toggle without a round-trip. */
  setCallsEnabled: (on: boolean) => void;
  /** Reflect a just-saved ringtone choice without a round-trip. */
  setRingtone: (id: RingtoneId) => void;
}

export const useSettings = create<SettingsStore>((set) => ({
  callsEnabled: false,
  ringtone: DEFAULT_RINGTONE,
  load: async () => {
    try {
      const s = await settingsApi.get();
      set({
        callsEnabled: s.calls_enabled,
        ringtone: asRingtoneId(s.ringtone),
      });
    } catch {
      // Settings unavailable (node not up yet) — keep the safe defaults.
    }
  },
  setCallsEnabled: (on) => set({ callsEnabled: on }),
  setRingtone: (id) => set({ ringtone: id }),
}));
