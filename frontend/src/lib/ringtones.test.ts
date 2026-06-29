import { describe, it, expect, vi, afterEach } from "vitest";
import {
  asRingtoneId,
  DEFAULT_RINGTONE,
  RINGTONE_IDS,
  playCallTone,
} from "./ringtones";

describe("asRingtoneId", () => {
  it("accepts every known ringtone id unchanged", () => {
    for (const id of RINGTONE_IDS) expect(asRingtoneId(id)).toBe(id);
  });

  it("falls back to the default for unknown / empty / missing values", () => {
    expect(asRingtoneId("nope")).toBe(DEFAULT_RINGTONE);
    expect(asRingtoneId("")).toBe(DEFAULT_RINGTONE);
    expect(asRingtoneId(undefined)).toBe(DEFAULT_RINGTONE);
  });

  it("defaults to the classic ring", () => {
    expect(DEFAULT_RINGTONE).toBe("classic");
  });
});

describe("playCallTone", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("degrades to a no-op stopper when Web Audio is unavailable", () => {
    // No AudioContext on the window (e.g. an old/locked-down webview) → must not throw.
    vi.stubGlobal("window", {});
    const stop = playCallTone("incoming", "classic");
    expect(typeof stop).toBe("function");
    expect(() => stop()).not.toThrow();
  });
});
