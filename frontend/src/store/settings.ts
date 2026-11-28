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
import {
  DEFAULT_SCREENSHOT_SHORTCUTS,
  type ScreenshotShortcuts,
} from "@/lib/screenshotShortcuts";
import {
  screenshotShortcutsApi,
  type ScreenshotRegistration,
} from "@/lib/api/screenshotShortcuts";
import { errorMessage } from "@/lib/error";
import { readWithDeadline } from "@/lib/readDeadline";

const emptyRegistration = (): ScreenshotRegistration => ({
  global_active: false,
  generation: null,
  registration_id: 0,
  error: null,
});
let registrationRequest = 0;
let muteRevision = 0;
let muteUpdate: Promise<void> = Promise.resolve();

/** App-setting flags the UI needs reactively (most settings are read in Rust, but a few —
 * like the experimental calls toggle and the ringtone — drive UI affordances / playback and
 * must re-render on change). */
interface SettingsStore {
  screenshotShortcuts: ScreenshotShortcuts;
  screenshotShortcutsLoaded: boolean;
  screenshotRegistration: ScreenshotRegistration;
  screenshotRegistrationPending: boolean;
  screenshotRecording: boolean;
  setScreenshotShortcuts: (value: ScreenshotShortcuts) => void;
  syncScreenshotShortcuts: () => Promise<ScreenshotRegistration | null>;
  owner: string | null;
  setOwner: (owner: string | null) => void;
  /** Experimental voice/video calls — off by default, opt-in via the Settings dialog. */
  callsEnabled: boolean;
  mutedGroups: string[];
  setGroupMuted: (id: string, muted: boolean) => Promise<void>;
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
  mutedGroups: [],
  setGroupMuted: (id, muted) => {
    const lease = captureRuntimeOwner();
    const current = () =>
      lease.owner !== null && lease.current() && get().owner === lease.owner;
    const task = muteUpdate.then(async () => {
      if (!current()) throw new Error("Settings session changed");
      const persisted = await readWithDeadline(() => settingsApi.get());
      if (!current()) throw new Error("Settings session changed");
      const groups = (persisted.muted_groups ?? []).filter(
        (group) => group !== id,
      );
      if (muted) groups.push(id);
      await settingsApi.update({ muted_groups: groups });
      if (current()) {
        muteRevision++;
        set({ mutedGroups: groups });
      }
    });
    muteUpdate = task.catch(() => {});
    return task;
  },
  owner: null,
  setOwner: (owner) => {
    if (get().owner === owner) return;
    registrationRequest++;
    muteRevision++;
    set({
      screenshotShortcuts: { ...DEFAULT_SCREENSHOT_SHORTCUTS },
      screenshotShortcutsLoaded: false,
      screenshotRegistration: emptyRegistration(),
      screenshotRegistrationPending: false,
      screenshotRecording: false,
      owner,
      callsEnabled: false,
      mutedGroups: [],
      notificationsEnabled: null,
      ringtone: DEFAULT_RINGTONE,
      privacy: balancedPrivacy,
    });
  },
  callsEnabled: false,
  screenshotShortcuts: { ...DEFAULT_SCREENSHOT_SHORTCUTS },
  screenshotShortcutsLoaded: false,
  screenshotRegistration: emptyRegistration(),
  screenshotRegistrationPending: false,
  screenshotRecording: false,
  setScreenshotShortcuts: (value) => {
    if (JSON.stringify(get().screenshotShortcuts) === JSON.stringify(value)) {
      set({ screenshotShortcutsLoaded: true });
      return;
    }
    registrationRequest++;
    set({
      screenshotShortcuts: value,
      screenshotShortcutsLoaded: true,
      screenshotRegistration: emptyRegistration(),
      screenshotRegistrationPending: false,
    });
  },
  syncScreenshotShortcuts: async () => {
    const lease = captureRuntimeOwner();
    if (!lease.owner || !lease.current() || get().owner !== lease.owner)
      return null;
    const request = ++registrationRequest;
    set({ screenshotRegistrationPending: true });
    const current = () =>
      request === registrationRequest &&
      lease.current() &&
      get().owner === lease.owner;
    try {
      const status = await screenshotShortcutsApi.sync(lease.owner);
      if (current())
        set({
          screenshotRegistration: status,
          screenshotRegistrationPending: false,
        });
      return status;
    } catch (error) {
      if (current())
        set({
          screenshotRegistration: {
            ...emptyRegistration(),
            error: errorMessage(error),
          },
          screenshotRegistrationPending: false,
        });
      return null;
    }
  },
  notificationsEnabled: null,
  ringtone: DEFAULT_RINGTONE,
  privacy: balancedPrivacy,
  load: async () => {
    const lease = captureRuntimeOwner();
    if (!lease.owner || !lease.current() || get().owner !== lease.owner) return;
    const revision = muteRevision;
    try {
      const s = await readWithDeadline(() => settingsApi.get());
      if (!lease.current() || get().owner !== lease.owner) return;
      set({
        screenshotShortcuts: s.screenshot_shortcuts ?? {
          ...DEFAULT_SCREENSHOT_SHORTCUTS,
        },
        screenshotShortcutsLoaded: true,
        callsEnabled: s.calls_enabled,
        ...(revision === muteRevision
          ? { mutedGroups: s.muted_groups ?? [] }
          : {}),
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
