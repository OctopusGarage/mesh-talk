// @vitest-environment jsdom
import { act, createElement as h } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { Tabs, TabsList, TabsTrigger } from "./tabs";

it("disables pointer motion immediately when keyboard navigation begins", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
      unobserve() {}
    },
  );
  const width = vi
    .spyOn(HTMLElement.prototype, "offsetWidth", "get")
    .mockReturnValue(80);
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  try {
    await act(async () =>
      root.render(
        h(
          Tabs,
          { defaultValue: "a" },
          h(
            TabsList,
            {},
            h(TabsTrigger, { value: "a" }, "First"),
            h(TabsTrigger, { value: "b" }, "Second"),
          ),
        ),
      ),
    );
    const list = host.querySelector('[role="tablist"]')!;
    await act(async () =>
      list.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true })),
    );
    expect(list.getAttribute("data-pointer-motion")).toBe("true");
    await act(async () =>
      list.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }),
      ),
    );
    expect(list.hasAttribute("data-pointer-motion")).toBe(false);
    expect(host.querySelector('[aria-hidden="true"]')).not.toBeNull();
  } finally {
    await act(async () => root.unmount());
    host.remove();
    width.mockRestore();
    vi.unstubAllGlobals();
  }
});
