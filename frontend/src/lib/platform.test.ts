import { afterEach, expect, it, vi } from "vitest";
import {
  applyPlatformClass,
  isMacOverlay,
  needsCustomWindowControls,
} from "./platform";

afterEach(() => vi.unstubAllGlobals());

it.each([
  { agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)", expected: "macos" },
  { agent: "Mozilla/5.0 (Windows NT 10.0)", expected: "frameless" },
  { agent: "Mozilla/5.0 (X11; Linux x86_64)", expected: "frameless" },
])(
  "selects $expected chrome inside the desktop webview",
  ({ agent, expected }) => {
    const setAttribute = vi.fn();
    vi.stubGlobal("window", { __TAURI_INTERNALS__: {} });
    vi.stubGlobal("navigator", { userAgent: agent });
    vi.stubGlobal("document", { documentElement: { setAttribute } });

    applyPlatformClass();

    expect(setAttribute).toHaveBeenCalledWith("data-os", expected);
    expect(isMacOverlay()).toBe(expected === "macos");
    expect(needsCustomWindowControls()).toBe(expected === "frameless");
  },
);

it("keeps browser preview free of native window insets", () => {
  const setAttribute = vi.fn();
  vi.stubGlobal("window", {});
  vi.stubGlobal("navigator", { userAgent: "Macintosh" });
  vi.stubGlobal("document", { documentElement: { setAttribute } });

  applyPlatformClass();

  expect(setAttribute).not.toHaveBeenCalled();
  expect(isMacOverlay()).toBe(false);
  expect(needsCustomWindowControls()).toBe(false);
});
