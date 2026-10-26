// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { installScreenshotShortcutRouter } from "./shortcutRouter";
import { DEFAULT_SCREENSHOT_SHORTCUTS } from "@/lib/screenshotShortcuts";
import type { ScreenshotShortcutEvent } from "@/lib/api/screenshotShortcuts";

let globalEvent: (value: ScreenshotShortcutEvent) => void;
let cleanup: ReturnType<typeof installScreenshotShortcutRouter>;
const capture = vi.fn();
const wake = vi.fn();
const unlisten = vi.fn();
let snapshot = {
  current: true,
  available: true,
  blocked: false,
  writable: true,
  globalActive: false,
  generation: 7,
  owner: "alice",
  bindings: { ...DEFAULT_SCREENSHOT_SHORTCUTS },
};
beforeEach(async () => {
  vi.resetAllMocks();
  vi.spyOn(document, "hasFocus").mockReturnValue(true);
  snapshot = {
    current: true,
    available: true,
    blocked: false,
    writable: true,
    globalActive: false,
    generation: 7,
    owner: "alice",
    bindings: { ...DEFAULT_SCREENSHOT_SHORTCUTS },
  };
  capture.mockResolvedValue(undefined);
  wake.mockResolvedValue(undefined);
  cleanup = installScreenshotShortcutRouter({
    read: () => snapshot,
    capture,
    wake,
    listen: async (callback) => {
      globalEvent = callback;
      return unlisten;
    },
    mac: false,
  });
  await cleanup.ready;
});
afterEach(() => {
  cleanup();
  document.body.innerHTML = "";
  vi.restoreAllMocks();
});
function press(patch: KeyboardEventInit = {}, target: EventTarget = window) {
  const event = new KeyboardEvent("keydown", {
    code: "KeyX",
    key: "X",
    ctrlKey: true,
    shiftKey: true,
    bubbles: true,
    cancelable: true,
    ...patch,
  });
  target.dispatchEvent(event);
  return event;
}
it("routes local bindings including when focus is in the composer", async () => {
  const input = document.createElement("textarea");
  document.body.append(input);
  expect(press({}, input).defaultPrevented).toBe(true);
  expect(capture).toHaveBeenCalledWith(false);
  await new Promise((resolve) => setTimeout(resolve, 0));
  press({ altKey: true });
  expect(capture).toHaveBeenLastCalledWith(true);
});
it("uses only OS events while global registration is effective", () => {
  snapshot.globalActive = true;
  press();
  expect(capture).not.toHaveBeenCalled();
  globalEvent({ owner: "alice", generation: 7, hide_window: true });
  expect(capture).toHaveBeenCalledWith(true);
});
it("rejects foreign owners, retired native generations and stale views", () => {
  snapshot.globalActive = true;
  globalEvent({ owner: "bob", generation: 7, hide_window: true });
  globalEvent({ owner: "alice", generation: 6, hide_window: true });
  snapshot.current = false;
  globalEvent({ owner: "alice", generation: 7, hide_window: true });
  press();
  expect(capture).not.toHaveBeenCalled();
});
it("does not steal modal, recording, composition or repeated keys", () => {
  press({ repeat: true });
  press({ isComposing: true });
  const modal = document.createElement("div");
  modal.setAttribute("role", "dialog");
  document.body.append(modal);
  press();
  snapshot.globalActive = true;
  globalEvent({ owner: "alice", generation: 7, hide_window: true });
  expect(capture).not.toHaveBeenCalled();
});
it("requires foreground focus locally and wakes with no capture when the backend is unavailable", () => {
  vi.spyOn(document, "hasFocus").mockReturnValue(false);
  press();
  snapshot.available = false;
  snapshot.globalActive = true;
  globalEvent({ owner: "alice", generation: 7, hide_window: true });
  expect(capture).not.toHaveBeenCalled();
  expect(wake).toHaveBeenCalledOnce();
});
it("wakes the app without capturing when no writable conversation is selected", () => {
  snapshot.writable = false;
  press();
  expect(wake).toHaveBeenCalledOnce();
  expect(capture).not.toHaveBeenCalled();
});
it("admits only one acquisition and removes both listeners on cleanup", () => {
  capture.mockReturnValue(new Promise(() => {}));
  press();
  press();
  expect(capture).toHaveBeenCalledOnce();
  cleanup();
  press();
  expect(capture).toHaveBeenCalledOnce();
  expect(unlisten).toHaveBeenCalledOnce();
});

it("keeps local shortcuts available while native listener setup is stalled", () => {
  cleanup();
  cleanup = installScreenshotShortcutRouter({
    read: () => snapshot,
    capture,
    wake,
    listen: () => new Promise(() => {}),
    mac: false,
  });
  press();
  expect(capture).toHaveBeenCalledWith(false);
});

it("keeps local shortcuts available when native listener setup fails", async () => {
  cleanup();
  const report = vi.fn();
  cleanup = installScreenshotShortcutRouter({
    read: () => snapshot,
    capture,
    wake,
    listen: async () => {
      throw new Error("events unavailable");
    },
    mac: false,
    report,
  });
  expect(await cleanup.ready).toBe(false);
  press();
  expect(capture).toHaveBeenCalledWith(false);
  expect(report).toHaveBeenCalledOnce();
});
