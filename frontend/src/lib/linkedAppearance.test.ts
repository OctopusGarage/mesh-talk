import { afterEach, beforeEach, expect, it, vi } from "vitest";

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
    getPolicy: vi.fn(),
    getPreferences: vi.fn(),
    setPreference: vi.fn(),
    apply: vi.fn((key: string, value: string | null) => {
      if (value !== null) values[key] = value;
      for (const listener of listeners) listener();
    }),
  };
});
vi.mock("@/lib/api/chat", () => ({
  chat: {
    getDeviceSyncPolicy: mocks.getPolicy,
    getAccountPreferences: mocks.getPreferences,
    setAccountPreference: mocks.setPreference,
  },
}));
vi.mock("@/lib/theme", () => ({
  appearanceValues: () => ({ ...mocks.values }),
  applyLinkedAppearance: mocks.apply,
  useTheme: {
    subscribe: (listener: () => void) => {
      mocks.listeners.add(listener);
      return () => mocks.listeners.delete(listener);
    },
  },
}));

import { startLinkedAppearance } from "./linkedAppearance";

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("window", globalThis);
  mocks.listeners.clear();
  mocks.values.theme = "dark";
  mocks.values.wallpaper_enabled = "on";
  mocks.values.pack_wallpaper_ids = "{}";
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
  mocks.setPreference.mockReset().mockResolvedValue({ revision: 11 });
  mocks.apply.mockClear();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it("adopts a linked theme without publishing a new device's default", async () => {
  const stop = startLinkedAppearance();
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
  const stop = startLinkedAppearance();
  await vi.waitFor(() => expect(mocks.getPreferences).toHaveBeenCalled());
  expect(mocks.apply).not.toHaveBeenCalled();
  stop();
});

it("publishes a later local theme edit once", async () => {
  const stop = startLinkedAppearance();
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
    ),
  );
  expect(mocks.setPreference).toHaveBeenCalledTimes(1);
  stop();
});

it("retries a failed local edit on the next poll", async () => {
  const stop = startLinkedAppearance();
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
