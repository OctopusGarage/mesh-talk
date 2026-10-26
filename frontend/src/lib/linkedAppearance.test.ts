import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useAuth } from "@/store/auth";
import { useSettings } from "@/store/settings";
import i18n, { setLanguage } from "./i18n";

const scope = vi.hoisted(() => ({ account: "source-crypto-account" }));

const mocks = vi.hoisted(() => {
  const listeners = new Set<() => void>();
  const values: Record<string, string> = {
    theme: "dark",
    wallpaper_enabled: "on",
    pack_wallpaper_ids: "{}",
  };
  return {
    listeners,
    values,
    packListeners: new Set<(state: { packs: unknown[] }) => void>(),
    packValues: [] as unknown[],
    refreshTheme: vi.fn(),
    saved: new Set<string>(),
    reconcileSettings: vi.fn(),
    preparePacks: vi.fn(),
    prepareAvatars: vi.fn(),
    getPolicy: vi.fn(),
    getPreferences: vi.fn(),
    setPreference: vi.fn(),
    seedPreference: vi.fn(),
    apply: vi.fn((key: string, value: string | null) => {
      if (value !== null) values[key] = value;
      for (const listener of listeners) listener();
    }),
  };
});
vi.mock("./linkedCustomizationPacks", () => ({
  startLinkedCustomizationPacks: () => () => {},
  publishCustomizationPacksBeforeLink: mocks.preparePacks,
}));
vi.mock("./linkedAvatarOverrides", () => ({
  startLinkedAvatarOverrides: () => () => {},
  publishAvatarOverridesBeforeLink: mocks.prepareAvatars,
}));
vi.mock("@/store/packs", () => ({
  usePacks: {
    getState: () => ({ packs: mocks.packValues }),
    subscribe: (listener: (state: { packs: unknown[] }) => void) => {
      mocks.packListeners.add(listener);
      return () => mocks.packListeners.delete(listener);
    },
  },
}));
vi.mock("@/store/chat", () => ({
  useChat: { getState: () => ({ myAccountId: scope.account }) },
}));
vi.mock("@/lib/api/chat", () => ({
  chat: {
    getDeviceSyncPolicy: mocks.getPolicy,
    getAccountPreferences: mocks.getPreferences,
    setAccountPreference: mocks.setPreference,
    seedAccountPreference: mocks.seedPreference,
  },
}));
vi.mock("@/lib/api/settings", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/api/settings")>()),
  reconcileAccountSettings: mocks.reconcileSettings,
}));
vi.mock("@/lib/theme", () => ({
  appearanceValues: () => ({ ...mocks.values }),
  savedAppearanceValues: () =>
    Object.fromEntries(
      Object.entries(mocks.values).filter(([key]) => mocks.saved.has(key)),
    ),
  applyLinkedAppearance: mocks.apply,
  useTheme: {
    getState: () => ({ refresh: mocks.refreshTheme }),
    subscribe: (listener: () => void) => {
      mocks.listeners.add(listener);
      return () => mocks.listeners.delete(listener);
    },
  },
}));

import {
  publishAppearanceBeforeLink,
  startLinkedAppearance,
} from "./linkedAppearance";

beforeEach(async () => {
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => stored.set(key, value),
    removeItem: (key: string) => stored.delete(key),
  });
  await i18n.changeLanguage("en");
  scope.account = "source-crypto-account";
  useSettings.getState().setOwner(null);
  useSettings.getState().setOwner("owner");
  useAuth.setState({
    user: { id: "owner", username: "Owner", display_name: "Owner" },
    generation: 1,
  });
  vi.useFakeTimers();
  vi.stubGlobal("window", globalThis);
  mocks.listeners.clear();
  mocks.packListeners.clear();
  mocks.packValues = [];
  mocks.refreshTheme.mockReset();
  mocks.saved.clear();
  mocks.values.theme = "dark";
  mocks.values.wallpaper_enabled = "on";
  mocks.values.pack_wallpaper_ids = "{}";
  mocks.preparePacks.mockReset().mockResolvedValue(undefined);
  mocks.prepareAvatars.mockReset().mockResolvedValue(undefined);
  mocks.reconcileSettings.mockReset().mockResolvedValue(null);
  mocks.getPolicy.mockReset().mockResolvedValue({ appearance: true });
  mocks.getPreferences.mockReset().mockResolvedValue([
    {
      category: "appearance",
      key: "theme",
      value: "light",
      revision: 10,
      device_id: "a".repeat(32),
    },
  ]);
  mocks.seedPreference.mockReset().mockResolvedValue({ revision: 11 });
  mocks.setPreference.mockReset().mockResolvedValue({ revision: 11 });
  mocks.apply.mockClear();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it("does not publish unsaved appearance defaults before pairing", async () => {
  mocks.getPreferences.mockResolvedValue([]);
  await publishAppearanceBeforeLink("owner");
  expect(mocks.seedPreference).not.toHaveBeenCalled();
});

it("publishes only explicitly saved local appearance before pairing", async () => {
  mocks.getPreferences.mockResolvedValue([]);
  mocks.values.theme = "oled";
  mocks.saved.add("theme");
  await publishAppearanceBeforeLink("owner");
  expect(mocks.seedPreference).toHaveBeenCalledExactlyOnceWith(
    "appearance",
    "theme",
    "oled",
    "owner",
    "source-crypto-account",
  );
});

it("preserves account values while publishing other saved appearance keys", async () => {
  mocks.saved.add("theme");
  mocks.saved.add("wallpaper_enabled");
  await publishAppearanceBeforeLink("owner");
  expect(mocks.seedPreference).toHaveBeenCalledExactlyOnceWith(
    "appearance",
    "wallpaper_enabled",
    "on",
    "owner",
    "source-crypto-account",
  );
});

it("adopts a linked theme without publishing a new device's default", async () => {
  const stop = startLinkedAppearance("owner");
  await vi.waitFor(() =>
    expect(mocks.apply).toHaveBeenCalledWith("theme", "light"),
  );
  expect(mocks.values.theme).toBe("light");
  expect(mocks.setPreference).not.toHaveBeenCalled();
  stop();
  expect(mocks.listeners.size).toBe(0);
});

it("keeps remote appearance local until this device enables its choice", async () => {
  mocks.getPolicy.mockResolvedValue({ appearance: false });
  const stop = startLinkedAppearance("owner");
  await vi.waitFor(() => expect(mocks.getPreferences).toHaveBeenCalled());
  expect(mocks.apply).not.toHaveBeenCalled();
  stop();
});

it("publishes a later local theme edit once", async () => {
  const stop = startLinkedAppearance("owner");
  await vi.waitFor(() =>
    expect(mocks.apply).toHaveBeenCalledWith("theme", "light"),
  );
  mocks.values.theme = "oled";
  for (const listener of mocks.listeners) listener();
  await vi.waitFor(() =>
    expect(mocks.setPreference).toHaveBeenCalledWith(
      "appearance",
      "theme",
      "oled",
      "owner",
      "source-crypto-account",
    ),
  );
  expect(mocks.setPreference).toHaveBeenCalledTimes(1);
  stop();
});

it("retries a failed local edit on the next poll", async () => {
  const stop = startLinkedAppearance("owner");
  await vi.waitFor(() =>
    expect(mocks.apply).toHaveBeenCalledWith("theme", "light"),
  );
  mocks.setPreference.mockRejectedValueOnce(new Error("offline"));
  mocks.values.theme = "oled";
  for (const listener of mocks.listeners) listener();
  await vi.waitFor(() => expect(mocks.setPreference).toHaveBeenCalledTimes(1));
  await vi.advanceTimersByTimeAsync(10_000);
  expect(mocks.setPreference).toHaveBeenCalledTimes(2);
  stop();
});

it("keeps the originating owner on every appearance read and write", async () => {
  useAuth.setState({
    user: { id: "owner-a", username: "Owner", display_name: "Owner" },
  });
  const stop = startLinkedAppearance("owner-a");
  await vi.waitFor(() => expect(mocks.apply).toHaveBeenCalled());
  expect(mocks.getPolicy).toHaveBeenCalledWith("owner-a");
  expect(mocks.getPreferences).toHaveBeenCalledWith("appearance", "owner-a");
  mocks.values.theme = "oled";
  for (const listener of mocks.listeners) listener();
  await vi.waitFor(() =>
    expect(mocks.setPreference).toHaveBeenCalledWith(
      "appearance",
      "theme",
      "oled",
      "owner-a",
      "source-crypto-account",
    ),
  );
  stop();
});

it("does not apply a pending appearance read after the cryptographic account changes", async () => {
  let finish!: (records: unknown[]) => void;
  mocks.getPreferences.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const stop = startLinkedAppearance("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    scope.account = "replacement-account";
    finish([
      { key: "theme", value: "light", revision: 10, device_id: "a".repeat(32) },
    ]);
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.apply).not.toHaveBeenCalled();
    mocks.values.theme = "oled";
    for (const listener of mocks.listeners) listener();
    expect(mocks.setPreference).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});

it("prepares native shared preferences before issuing a pairing code", async () => {
  await publishAppearanceBeforeLink("owner");
  expect(mocks.reconcileSettings).toHaveBeenCalledWith(
    "owner",
    "source-crypto-account",
    true,
  );
});

it("reconciles native settings through the same account-bound appearance poll", async () => {
  const stop = startLinkedAppearance("owner");
  await vi.waitFor(() =>
    expect(mocks.reconcileSettings).toHaveBeenCalledWith(
      "owner",
      "source-crypto-account",
    ),
  );
  stop();
});

it("refreshes native notification and ringtone presentation without echoing preferences", async () => {
  mocks.reconcileSettings.mockResolvedValue({
    calls_enabled: true,
    notifications: false,
    ringtone: "marimba",
    privacy_level: "custom",
    notification_preview: false,
    auto_lock_seconds: 60,
    lock_when_hidden: true,
    confirm_sensitive_copy: true,
    clipboard_clear_seconds: 30,
  });
  const stop = startLinkedAppearance("owner");
  await vi.waitFor(() =>
    expect(useSettings.getState().ringtone).toBe("marimba"),
  );
  expect(useSettings.getState().notificationsEnabled).toBe(false);
  expect(useSettings.getState().privacy.auto_lock_seconds).toBe(60);
  expect(mocks.setPreference).not.toHaveBeenCalled();
  stop();
});

it("ignores native projection after a same-owner session replacement", async () => {
  let resolve!: (value: unknown) => void;
  mocks.reconcileSettings.mockImplementationOnce(
    () =>
      new Promise((finish) => {
        resolve = finish;
      }),
  );
  const stop = startLinkedAppearance("owner");
  await vi.advanceTimersByTimeAsync(0);
  useAuth.setState({ generation: 2 });
  resolve({ ringtone: "marimba", notifications: false });
  await vi.advanceTimersByTimeAsync(0);
  expect(useSettings.getState().ringtone).toBe("classic");
  expect(mocks.apply).not.toHaveBeenCalled();
  stop();
});

it("does not attribute legacy assets after the account changes during native source preparation", async () => {
  let finish!: () => void;
  mocks.reconcileSettings.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  const pending = publishAppearanceBeforeLink("owner");
  scope.account = "replacement-account";
  finish();
  await expect(pending).rejects.toThrow(/Account changed/);
  expect(mocks.preparePacks).not.toHaveBeenCalled();
  expect(mocks.prepareAvatars).not.toHaveBeenCalled();
  expect(mocks.seedPreference).not.toHaveBeenCalled();
});

it("refreshes late imported theme assets without publishing a new scalar preference", async () => {
  const stop = startLinkedAppearance("owner");
  await vi.waitFor(() =>
    expect(mocks.apply).toHaveBeenCalledWith("theme", "light"),
  );
  mocks.packValues = [{ id: "imported-theme" }];
  for (const listener of mocks.packListeners)
    listener({ packs: mocks.packValues });
  expect(mocks.refreshTheme).toHaveBeenCalledWith(true);
  expect(mocks.setPreference).not.toHaveBeenCalled();
  stop();
  expect(mocks.packListeners.size).toBe(0);
});

it("applies a valid remote language without echoing it as a local edit", async () => {
  mocks.getPreferences.mockResolvedValue([
    { key: "language", value: "es", revision: 10, device_id: "a".repeat(32) },
  ]);
  const stop = startLinkedAppearance("owner");
  try {
    await vi.waitFor(() => expect(i18n.language).toBe("es"));
    expect(mocks.setPreference).not.toHaveBeenCalled();
    expect(localStorage.getItem("mesh-talk-lang")).toBe("es");
    i18n.emit("languageChanged", "es");
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.setPreference).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});
it("publishes an explicit local language change once and disposes its subscription", async () => {
  const stop = startLinkedAppearance("owner");
  try {
    await vi.waitFor(() => expect(mocks.apply).toHaveBeenCalled());
    setLanguage("ja");
    await vi.waitFor(() =>
      expect(mocks.setPreference).toHaveBeenCalledExactlyOnceWith(
        "appearance",
        "language",
        "ja",
        "owner",
        "source-crypto-account",
      ),
    );
  } finally {
    stop();
  }
  mocks.setPreference.mockClear();
  setLanguage("es");
  await vi.advanceTimersByTimeAsync(0);
  expect(mocks.setPreference).not.toHaveBeenCalled();
});
it("does not seed an implicit source locale as an account preference", async () => {
  await i18n.changeLanguage("ja");
  mocks.getPreferences.mockResolvedValue([]);
  await publishAppearanceBeforeLink("owner");
  expect(mocks.seedPreference).not.toHaveBeenCalled();
});

it("seeds only an explicitly stored source language and preserves canonical locale", async () => {
  mocks.getPreferences.mockResolvedValue([]);
  setLanguage("zh-Hant");
  await publishAppearanceBeforeLink("owner");
  expect(mocks.seedPreference).toHaveBeenCalledExactlyOnceWith(
    "appearance",
    "language",
    "zh-Hant",
    "owner",
    "source-crypto-account",
  );
  mocks.seedPreference.mockClear();
  mocks.getPreferences.mockResolvedValue([
    { key: "language", value: null, revision: 10, device_id: "a".repeat(32) },
  ]);
  await publishAppearanceBeforeLink("owner");
  expect(mocks.seedPreference).not.toHaveBeenCalled();
});
it("validates remote languages and resets a tombstone to canonical English", async () => {
  mocks.getPreferences.mockResolvedValue([
    { key: "language", value: "fr", revision: 10, device_id: "a".repeat(32) },
  ]);
  const stop = startLinkedAppearance("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(i18n.language).toBe("en");
    mocks.getPreferences.mockResolvedValue([
      { key: "language", value: "ja", revision: 11, device_id: "a".repeat(32) },
    ]);
    await vi.advanceTimersByTimeAsync(10000);
    expect(i18n.language).toBe("ja");
    mocks.getPreferences.mockResolvedValue([
      { key: "language", value: null, revision: 12, device_id: "a".repeat(32) },
    ]);
    await vi.advanceTimersByTimeAsync(10000);
    expect(i18n.language).toBe("en");
    expect(mocks.setPreference).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});
it("rejects delayed locale imports and local events after the runtime account changes", async () => {
  let finish!: (records: unknown[]) => void;
  mocks.getPreferences.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const stop = startLinkedAppearance("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    scope.account = "replacement-account";
    finish([
      { key: "language", value: "ja", revision: 10, device_id: "a".repeat(32) },
    ]);
    await vi.advanceTimersByTimeAsync(0);
    expect(i18n.language).toBe("en");
    setLanguage("es");
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.setPreference).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});

it("revokes rendered private theme CSS when its account appearance lifecycle ends", () => {
  const stop = startLinkedAppearance("owner");
  mocks.refreshTheme.mockClear();
  stop();
  expect(mocks.refreshTheme).toHaveBeenCalledWith(true);
});
