// @vitest-environment jsdom
import { act, createElement as h, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "./context-menu";
import { Popover, PopoverAnchor, PopoverContent } from "./popover";

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  await act(async () => vi.runOnlyPendingTimers());
  host.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
function element(id: string) {
  const found = document.querySelector<HTMLElement>(`[data-testid="${id}"]`);
  expect(found, `missing ${id}`).not.toBeNull();
  return found!;
}
type HandoffProps = {
  openPicker?: boolean;
  onCloseAutoFocus?: (event: Event) => void;
};
function Handoff({ openPicker = true, onCloseAutoFocus }: HandoffProps) {
  const [open, setOpen] = useState(false);
  const [selected, setSelected] = useState(false);
  return h(
    "div",
    null,
    h("button", { "data-testid": "previous-owner" }, "Reply"),
    h("button", { "data-testid": "custom-owner" }, "Custom destination"),
    h(
      ContextMenu,
      null,
      h(
        ContextMenuTrigger,
        { asChild: true },
        h("button", { "data-testid": "menu-trigger" }, "Message"),
      ),
      h(
        ContextMenuContent,
        { ...{ "data-testid": "menu" }, onCloseAutoFocus },
        h(
          ContextMenuItem,
          {
            ...{ "data-testid": "react" },
            onSelect: () => {
              if (openPicker) setOpen(true);
            },
          },
          "React",
        ),
      ),
    ),
    h(
      Popover,
      { open, onOpenChange: setOpen },
      h(PopoverAnchor, { asChild: true }, h("span")),
      h(
        PopoverContent,
        { className: "", ...{ "data-testid": "picker" } },
        h(
          "button",
          {
            "data-testid": "emoji",
            onClick: () => {
              setSelected(true);
              setOpen(false);
            },
          },
          "🔥",
        ),
      ),
    ),
    selected && h("span", { "data-testid": "chip" }, "🔥"),
  );
}
async function openMenu(props: HandoffProps = {}) {
  await act(async () => root.render(h(Handoff, props)));
  element("previous-owner").focus();
  await act(async () =>
    element("menu-trigger").dispatchEvent(
      new MouseEvent("contextmenu", {
        bubbles: true,
        button: 2,
        clientX: 20,
        clientY: 20,
      }),
    ),
  );
  await act(async () => vi.runOnlyPendingTimers());
}
it("preserves a newer reaction popover when the old menu's deferred close completes", async () => {
  await openMenu();
  await act(async () => element("react").click());
  // The real menu has unmounted; its FocusScope close timer is still held.
  expect(document.querySelector('[data-testid="menu"]')).toBeNull();
  expect(element("picker").getAttribute("data-state")).toBe("open");
  expect(document.activeElement).toBe(element("emoji"));
  await act(async () => vi.runOnlyPendingTimers());
  expect(element("picker").getAttribute("data-state")).toBe("open");
  expect(document.activeElement).toBe(element("emoji"));
  await act(async () => element("emoji").click());
  expect(element("chip").textContent).toBe("🔥");
});

it("restores the previous owner after ordinary menu selection", async () => {
  await openMenu({ openPicker: false });
  await act(async () => element("react").click());
  expect(document.querySelector('[data-testid="menu"]')).toBeNull();
  await act(async () => vi.runOnlyPendingTimers());
  expect(document.activeElement).toBe(element("previous-owner"));
});
it("lets a menu consumer choose and cancel close focus before the shared guard", async () => {
  const onCloseAutoFocus = vi.fn((event: Event) => {
    expect(event.defaultPrevented).toBe(false);
    expect(document.activeElement).toBe(element("emoji"));
    element("custom-owner").focus();
    event.preventDefault();
  });
  await openMenu({ onCloseAutoFocus });
  await act(async () => element("react").click());
  await act(async () => vi.runOnlyPendingTimers());
  expect(onCloseAutoFocus).toHaveBeenCalledOnce();
  expect(document.activeElement).toBe(element("custom-owner"));
});
it("retains ordinary restoration when a menu consumer does not cancel", async () => {
  const onCloseAutoFocus = vi.fn();
  await openMenu({ openPicker: false, onCloseAutoFocus });
  await act(async () => element("react").click());
  await act(async () => vi.runOnlyPendingTimers());
  expect(onCloseAutoFocus).toHaveBeenCalledOnce();
  expect(document.activeElement).toBe(element("previous-owner"));
});
