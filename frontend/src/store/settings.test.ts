import { describe, it, expect, vi, beforeEach } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { useSettings } from "./settings";
import { DEFAULT_RINGTONE } from "@/lib/ringtones";

beforeEach(() => {
  invoke.mockReset();
  useSettings.setState({ callsEnabled: false, ringtone: DEFAULT_RINGTONE });
});

describe("useSettings.load", () => {
  it("maps calls_enabled and the (valid) ringtone from persisted settings", async () => {
    invoke.mockResolvedValue({ calls_enabled: true, ringtone: "marimba" });
    await useSettings.getState().load();
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
