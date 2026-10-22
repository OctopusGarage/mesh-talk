import { describe, it, expect, vi, beforeEach } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { useSettings } from "./settings";
import { DEFAULT_RINGTONE } from "@/lib/ringtones";
import { useAuth } from "./auth";

beforeEach(() => {
  invoke.mockReset();
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
    generation: 0,
  });
  useSettings.getState().setOwner(null);
  useSettings.getState().setOwner("alice");
  useSettings.setState({ callsEnabled: false, ringtone: DEFAULT_RINGTONE });
});

it("does not install an old owner's delayed settings", async () => {
  let finish!: (value: {
    calls_enabled: boolean;
    ringtone: string;
    notifications: boolean;
  }) => void;
  invoke.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = useSettings.getState().load();
  useAuth.setState({
    user: { id: "bob", username: "bob", display_name: "Bob" },
    generation: 1,
  });
  useSettings.getState().setOwner("bob");
  finish({ calls_enabled: true, ringtone: "marimba", notifications: true });
  await pending;
  expect(useSettings.getState().callsEnabled).toBe(false);
  expect(useSettings.getState().ringtone).toBe(DEFAULT_RINGTONE);
  expect(useSettings.getState().notificationsEnabled).toBeNull();
});

describe("useSettings.load", () => {
  it("maps calls_enabled and the (valid) ringtone from persisted settings", async () => {
    invoke.mockResolvedValue({
      calls_enabled: true,
      ringtone: "marimba",
      notifications: false,
    });
    await useSettings.getState().load();
    expect(useSettings.getState().notificationsEnabled).toBe(false);
    expect(useSettings.getState().callsEnabled).toBe(true);
    expect(useSettings.getState().ringtone).toBe("marimba");
  });

  it("coerces an unknown persisted ringtone to the default", async () => {
    invoke.mockResolvedValue({ calls_enabled: false, ringtone: "garbage" });
    await useSettings.getState().load();
    expect(useSettings.getState().ringtone).toBe(DEFAULT_RINGTONE);
  });

  it("keeps safe defaults when settings can't be read (node not up yet)", async () => {
    invoke.mockRejectedValue(new Error("not started"));
    await useSettings.getState().load();
    expect(useSettings.getState().callsEnabled).toBe(false);
    expect(useSettings.getState().ringtone).toBe(DEFAULT_RINGTONE);
  });
});

describe("useSettings setters", () => {
  it("setCallsEnabled / setRingtone update state reactively", () => {
    useSettings.getState().setCallsEnabled(true);
    useSettings.getState().setRingtone("digital");
    expect(useSettings.getState().callsEnabled).toBe(true);
    expect(useSettings.getState().ringtone).toBe("digital");
  });
});

it("loads every presentation privacy field and restores safe defaults on owner replacement", async () => {
  const privacy = {
    privacy_level: "custom" as const,
    notification_preview: false,
    auto_lock_seconds: 120,
    lock_when_hidden: true,
    confirm_sensitive_copy: true,
    clipboard_clear_seconds: 45,
  };
  invoke.mockResolvedValue({
    calls_enabled: false,
    ringtone: "digital",
    ...privacy,
  });
  await useSettings.getState().load();
  expect(useSettings.getState().privacy).toEqual(privacy);
  useSettings.getState().setOwner("bob");
  expect(useSettings.getState().privacy).toEqual({
    privacy_level: "balanced",
    notification_preview: true,
    auto_lock_seconds: 0,
    lock_when_hidden: false,
    confirm_sensitive_copy: false,
    clipboard_clear_seconds: 0,
  });
});
