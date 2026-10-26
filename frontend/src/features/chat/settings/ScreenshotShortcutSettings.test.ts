// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { DEFAULT_SCREENSHOT_SHORTCUTS } from "@/lib/screenshotShortcuts";
import { ScreenshotShortcutSettings } from "./ScreenshotShortcutSettings";
import type { SettingsController } from "./useSettingsController";
import { useSettings } from "@/store/settings";
import { screenshotShortcutsApi } from "@/lib/api/screenshotShortcuts";
import { useAuth } from "@/store/auth";

let root: Root;
let host: HTMLDivElement;
let controller: SettingsController;
const update = vi.fn();
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  await i18n.changeLanguage("en");
  update.mockReset().mockResolvedValue(true);
  useSettings.getState().setOwner(null);
  controller = {
    identity: "alice:1",
    settings: { screenshot_shortcuts: { ...DEFAULT_SCREENSHOT_SHORTCUTS } },
    saving: false,
    update,
  } as unknown as SettingsController;
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
async function render() {
  await act(async () =>
    root.render(createElement(ScreenshotShortcutSettings, { controller })),
  );
}
async function click(selector: string) {
  await act(async () =>
    host.querySelector<HTMLButtonElement>(selector)!.click(),
  );
}
async function record(
  code: string,
  modifiers = { ctrlKey: true, shiftKey: true },
) {
  const event = new KeyboardEvent("keydown", {
    bubbles: true,
    cancelable: true,
    code,
    key: code.slice(3).toLowerCase(),
    ...modifiers,
  });
  await act(async () =>
    host.querySelector("[data-shortcut-recording]")!.dispatchEvent(event),
  );
  return event;
}

it("records a custom binding without starting a screenshot and saves only the candidate", async () => {
  await render();
  await click("#setting-screenshot-capture");
  expect((await record("KeyB")).defaultPrevented).toBe(true);
  expect(update).toHaveBeenCalledWith(
    {
      screenshot_shortcuts: {
        ...DEFAULT_SCREENSHOT_SHORTCUTS,
        capture: "CommandOrControl+Shift+KeyB",
      },
    },
    "general",
  );
});
it("keeps recording after a duplicate and explains the conflict", async () => {
  await render();
  await click("#setting-screenshot-hidden");
  await record("KeyX");
  expect(update).not.toHaveBeenCalled();
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "different shortcut",
  );
  expect(host.querySelector("[data-shortcut-recording]")).not.toBeNull();
});
it("rejects editing shortcuts and lets Escape cancel without changing the binding", async () => {
  await render();
  await click("#setting-screenshot-capture");
  await record("KeyC", { ctrlKey: true, shiftKey: false });
  expect(update).not.toHaveBeenCalled();
  await act(async () =>
    host.querySelector("[data-shortcut-recording]")!.dispatchEvent(
      new KeyboardEvent("keydown", {
        code: "Escape",
        key: "Escape",
        bubbles: true,
        cancelable: true,
      }),
    ),
  );
  expect(host.querySelector("[data-shortcut-recording]")).toBeNull();
});
it("clears one binding and restores both defaults through the acknowledged save path", async () => {
  await render();
  await click('[data-testid="clear-screenshot-capture"]');
  expect(update).toHaveBeenLastCalledWith(
    {
      screenshot_shortcuts: { ...DEFAULT_SCREENSHOT_SHORTCUTS, capture: null },
    },
    "general",
  );
  await click('[data-testid="reset-screenshot-shortcuts"]');
  expect(update).toHaveBeenLastCalledWith(
    { screenshot_shortcuts: { ...DEFAULT_SCREENSHOT_SHORTCUTS } },
    "general",
  );
});
it("requires explicit opt-in for global activation", async () => {
  await render();
  expect(
    host
      .querySelector("#setting-screenshot-global")
      ?.getAttribute("aria-checked"),
  ).toBe("false");
  await click("#setting-screenshot-global");
  expect(update).toHaveBeenCalledWith(
    { screenshot_shortcuts: { ...DEFAULT_SCREENSHOT_SHORTCUTS, global: true } },
    "general",
  );
});

it("waits for native key ownership to be released before recording an existing global binding", async () => {
  useSettings.getState().setOwner("alice");
  let release!: () => void;
  const pause = vi.spyOn(screenshotShortcutsApi, "pause").mockReturnValue(
    new Promise((resolve) => {
      release = resolve;
    }),
  );
  await render();
  await click("#setting-screenshot-capture");
  expect(pause).toHaveBeenCalledWith("alice");
  expect(useSettings.getState().screenshotRecording).toBe(true);
  await record("KeyB");
  expect(update).not.toHaveBeenCalled();
  await act(async () => release());
  await record("KeyB");
  expect(update).toHaveBeenCalledOnce();
  expect(useSettings.getState().screenshotRecording).toBe(false);
});

it("reconciles bindings after an abandoned native pause completes late", async () => {
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
    generation: 1,
  });
  useSettings.getState().setOwner("alice");
  const sync = vi
    .spyOn(useSettings.getState(), "syncScreenshotShortcuts")
    .mockResolvedValue(null);
  let release!: () => void;
  vi.spyOn(screenshotShortcutsApi, "pause").mockReturnValue(
    new Promise((resolve) => {
      release = resolve;
    }),
  );
  await render();
  await click("#setting-screenshot-capture");
  await act(async () =>
    host
      .querySelector("[data-shortcut-recording]")!
      .dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
  );
  expect(sync).not.toHaveBeenCalled();
  await act(async () => release());
  expect(sync).toHaveBeenCalledOnce();
});
