// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { useSidebarWidth } from "./useSidebarWidth";

afterEach(() => vi.restoreAllMocks());

it("keeps resizing usable when local storage reads and writes are denied", async () => {
  vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
    throw new DOMException("denied", "SecurityError");
  });
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
    throw new DOMException("quota", "QuotaExceededError");
  });
  const element = document.createElement("div");
  const root = createRoot(element);
  function Probe() {
    const { width, onKeyDown, reset } = useSidebarWidth();
    return createElement("button", { onKeyDown, onDoubleClick: reset }, width);
  }
  try {
    await act(async () => root.render(createElement(Probe)));
    expect(element.textContent).toBe("284");
    await act(async () =>
      element
        .querySelector("button")!
        .dispatchEvent(
          new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }),
        ),
    );
    expect(element.textContent).toBe("300");
    await act(async () =>
      element
        .querySelector("button")!
        .dispatchEvent(new MouseEvent("dblclick", { bubbles: true })),
    );
    expect(element.textContent).toBe("284");
  } finally {
    await act(async () => root.unmount());
  }
});
