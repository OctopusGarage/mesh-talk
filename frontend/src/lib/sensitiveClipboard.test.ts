// @vitest-environment jsdom
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { useSettings } from "@/store/settings";
import { balancedPrivacy } from "@/store/settings";
import { copySensitiveText } from "./sensitiveClipboard";

let clipboardValue = "";
const writeText = vi.fn(async (value: string) => {
  clipboardValue = value;
});
const readText = vi.fn(async () => clipboardValue);

beforeEach(() => {
  vi.useFakeTimers();
  clipboardValue = "";
  writeText.mockClear();
  readText.mockClear();
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText, readText },
  });
  useSettings.setState({ privacy: balancedPrivacy });
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it("preserves a newer user copy when the clearing timer fires", async () => {
  useSettings.setState({
    privacy: { ...balancedPrivacy, clipboard_clear_seconds: 30 },
  });
  await copySensitiveText("secret", "confirm");
  clipboardValue = "newer copy";
  await vi.advanceTimersByTimeAsync(30_000);
  expect(clipboardValue).toBe("newer copy");
  expect(writeText).toHaveBeenCalledTimes(1);
});

it("clears its own unchanged copy after the configured delay", async () => {
  useSettings.setState({
    privacy: { ...balancedPrivacy, clipboard_clear_seconds: 30 },
  });
  await copySensitiveText("secret", "confirm");
  await vi.advanceTimersByTimeAsync(30_000);
  expect(clipboardValue).toBe("");
});

it("requires confirmation in Strict mode", async () => {
  useSettings.setState({
    privacy: { ...balancedPrivacy, confirm_sensitive_copy: true },
  });
  vi.spyOn(window, "confirm").mockReturnValue(false);
  expect(await copySensitiveText("secret", "confirm")).toBe(false);
  expect(writeText).not.toHaveBeenCalled();
});

it("supports synchronous native copy-event writes while retaining clipboard clearing", async () => {
  useSettings.setState({
    privacy: { ...balancedPrivacy, clipboard_clear_seconds: 30 },
  });
  const eventWrite = vi.fn((value: string) => {
    clipboardValue = value;
  });
  const copied = copySensitiveText("image text", "confirm", eventWrite);
  // Native event.clipboardData must be populated before its event returns.
  expect(eventWrite).toHaveBeenCalledWith("image text");
  expect(writeText).not.toHaveBeenCalled();
  expect(await copied).toBe(true);
  await vi.advanceTimersByTimeAsync(30_000);
  expect(clipboardValue).toBe("");
});
