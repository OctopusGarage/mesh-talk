// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { useConversationNavigation } from "./useConversationNavigation";

afterEach(() => vi.restoreAllMocks());

it("moves focus between sections and leaves unrelated buttons to their own keyboard handling", async () => {
  const element = document.createElement("div");
  document.body.append(element);
  const root = createRoot(element);
  function Probe() {
    const nav = useConversationNavigation(1, 1, 3);
    return createElement(
      "nav",
      { ref: nav.attachNav, onKeyDown: nav.onNavKeyDown },
      ...[1, 2, 3].map((position) =>
        createElement(
          "button",
          {
            key: position,
            "data-conv-option": true,
            "data-nav-position": position,
          },
          position,
        ),
      ),
      createElement("button", { id: "action" }, "More"),
    );
  }
  try {
    await act(async () => root.render(createElement(Probe)));
    const buttons = element.querySelectorAll("button");
    buttons[0].focus();
    await act(async () =>
      buttons[0].dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
      ),
    );
    expect(document.activeElement).toBe(buttons[1]);
    await act(async () =>
      buttons[1].dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }),
      ),
    );
    expect(document.activeElement).toBe(buttons[0]);
    buttons[3].focus();
    await act(async () =>
      buttons[3].dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
      ),
    );
    expect(document.activeElement).toBe(buttons[3]);
  } finally {
    await act(async () => root.unmount());
    element.remove();
  }
});

it("scrolls to an unmounted section row and does not steal focus from the composer when it appears", async () => {
  const frames: FrameRequestCallback[] = [];
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
    frames.push(callback);
    return frames.length;
  });
  const scroll = vi.fn();
  const element = document.createElement("div");
  document.body.append(element);
  const root = createRoot(element);
  function Probe({ showTarget = false }: { showTarget?: boolean }) {
    const nav = useConversationNavigation(1, 2, 4);
    // Only the Virtuoso imperative scrolling adapter is substituted.
    nav.virtualAccountsRef.current = {
      scrollToIndex: scroll,
    } as unknown as NonNullable<typeof nav.virtualAccountsRef.current>;
    return createElement(
      "div",
      null,
      createElement(
        "nav",
        { ref: nav.attachNav, onKeyDown: nav.onNavKeyDown },
        createElement(
          "button",
          { "data-conv-option": true, "data-nav-position": 1 },
          "Pin",
        ),
        showTarget &&
          createElement(
            "button",
            { "data-conv-option": true, "data-nav-position": 2 },
            "Contact",
          ),
      ),
      createElement("input", { id: "composer" }),
      createElement(
        "button",
        {
          id: "range",
          onClick: () => nav.focusVirtualRow({ startIndex: 0, endIndex: 1 }, 2),
        },
        "Range",
      ),
    );
  }
  try {
    await act(async () => root.render(createElement(Probe)));
    const first = element.querySelector("button")!;
    first.focus();
    await act(async () =>
      first.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
      ),
    );
    expect(scroll).toHaveBeenCalledWith({ index: 0, align: "center" });
    await act(async () =>
      root.render(createElement(Probe, { showTarget: true })),
    );
    const range = element.querySelector<HTMLButtonElement>("#range")!;
    await act(async () => range.click());
    await act(async () => frames.splice(0).forEach((callback) => callback(0)));
    expect(document.activeElement).toBe(
      element.querySelector('[data-nav-position="2"]'),
    );
    first.focus();
    // The target is now mounted; remove it so the next movement schedules a virtual focus.
    await act(async () => root.render(createElement(Probe)));
    await act(async () =>
      first.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
      ),
    );
    await act(async () =>
      root.render(createElement(Probe, { showTarget: true })),
    );
    const composer = element.querySelector("input")!;
    composer.focus();
    await act(async () => range.click());
    await act(async () => frames.splice(0).forEach((callback) => callback(0)));
    expect(document.activeElement).toBe(composer);
  } finally {
    await act(async () => root.unmount());
    element.remove();
  }
});
