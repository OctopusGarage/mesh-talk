import { describe, expect, it } from "vitest";
import {
  DEFAULT_SCREENSHOT_SHORTCUTS,
  bindingFromKeyboard,
  bindingProblem,
  formatShortcut,
  matchesShortcut,
  screenshotShortcutProblem,
  editorToolForKey,
} from "./screenshotShortcuts";

const key = (patch: Partial<KeyboardEvent> = {}) =>
  ({
    code: "KeyX",
    key: "x",
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    repeat: false,
    isComposing: false,
    ...patch,
  }) as KeyboardEvent;

describe("capture shortcut bindings", () => {
  it("records a portable primary modifier and exact modifier combination", () => {
    expect(
      bindingFromKeyboard(key({ ctrlKey: true, shiftKey: true }), false),
    ).toBe("CommandOrControl+Shift+KeyX");
    expect(
      bindingFromKeyboard(key({ metaKey: true, altKey: true }), true),
    ).toBe("CommandOrControl+Alt+KeyX");
    expect(
      bindingFromKeyboard(key({ metaKey: true, ctrlKey: true }), true),
    ).toBeNull();
  });
  it("requires a modifier and refuses composing, repeating or unsupported keys", () => {
    expect(bindingFromKeyboard(key(), false)).toBeNull();
    expect(
      bindingFromKeyboard(key({ ctrlKey: true, isComposing: true }), false),
    ).toBeNull();
    expect(
      bindingFromKeyboard(key({ ctrlKey: true, repeat: true }), false),
    ).toBeNull();
    expect(
      bindingFromKeyboard(key({ ctrlKey: true, code: "Enter" }), false),
    ).toBeNull();
  });
  it("matches the primary modifier for each platform without swallowing extra modifiers", () => {
    const binding = DEFAULT_SCREENSHOT_SHORTCUTS.capture;
    expect(
      matchesShortcut(key({ ctrlKey: true, shiftKey: true }), binding, false),
    ).toBe(true);
    expect(
      matchesShortcut(key({ metaKey: true, shiftKey: true }), binding, true),
    ).toBe(true);
    expect(
      matchesShortcut(key({ ctrlKey: true, shiftKey: true }), binding, true),
    ).toBe(false);
    expect(
      matchesShortcut(
        key({ ctrlKey: true, shiftKey: true, altKey: true }),
        binding,
        false,
      ),
    ).toBe(false);
    expect(
      matchesShortcut(
        key({ ctrlKey: true, shiftKey: true, repeat: true }),
        binding,
        false,
      ),
    ).toBe(false);
  });
  it("displays native modifier names and valid accessibility shortcut syntax", () => {
    expect(formatShortcut("CommandOrControl+Alt+Shift+KeyX", true)).toBe(
      "⌘⌥⇧X",
    );
    expect(formatShortcut("CommandOrControl+Shift+KeyX", false)).toBe(
      "Ctrl+Shift+X",
    );
    expect(formatShortcut(null, false)).toBe("");
  });
  it("rejects reserved editing bindings and malformed canonical bindings", () => {
    expect(bindingProblem("CommandOrControl+KeyC")).toBe("reserved");
    expect(bindingProblem("CommandOrControl+KeyQ")).toBe("reserved");
    expect(bindingProblem("Alt+F4")).toBe("reserved");
    expect(bindingProblem("Shift+KeyX")).toBe("invalid");
    expect(bindingProblem("Control+Control+KeyX")).toBe("invalid");
    expect(bindingProblem("CommandOrControl+Shift+KeyX")).toBeNull();
    expect(
      matchesShortcut(
        key({ code: "KeyC", ctrlKey: true }),
        "CommandOrControl+KeyC",
        false,
      ),
    ).toBe(false);
  });
  it("rejects duplicate bindings including equivalent platform modifiers", () => {
    expect(
      screenshotShortcutProblem(
        {
          ...DEFAULT_SCREENSHOT_SHORTCUTS,
          capture_hidden: DEFAULT_SCREENSHOT_SHORTCUTS.capture,
        },
        false,
      ),
    ).toBe("duplicate");
    expect(
      screenshotShortcutProblem(
        {
          ...DEFAULT_SCREENSHOT_SHORTCUTS,
          capture_hidden: "Control+Shift+KeyX",
        },
        false,
      ),
    ).toBe("duplicate");
    expect(
      screenshotShortcutProblem(
        { capture: null, capture_hidden: null, global: true },
        false,
      ),
    ).toBe("empty");
    expect(
      screenshotShortcutProblem(
        { capture: null, capture_hidden: null, global: false },
        false,
      ),
    ).toBeNull();
  });
});

it("maps editor tool keys without stealing modified keys, repeat or IME input", () => {
  expect(editorToolForKey(key({ code: "KeyR", key: "r" }))).toBe("rect");
  expect(editorToolForKey(key({ code: "KeyM", key: "m" }))).toBe("mosaic");
  expect(editorToolForKey(key({ code: "KeyR", ctrlKey: true }))).toBeNull();
  expect(editorToolForKey(key({ code: "KeyR", isComposing: true }))).toBeNull();
  expect(editorToolForKey(key({ code: "KeyR", repeat: true }))).toBeNull();
});
