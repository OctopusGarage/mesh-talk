// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement as h } from "react";
import { createRoot, type Root } from "react-dom/client";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  DialogTrigger,
} from "./dialog";

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
  document.documentElement.removeAttribute("tabindex");
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
function element(id: string) {
  const found = document.querySelector<HTMLElement>(`[data-testid="${id}"]`);
  expect(found, `missing ${id}`).not.toBeNull();
  return found!;
}
function dialog(
  id: string,
  contentProps: React.ComponentProps<typeof DialogContent> = {},
  children?: React.ReactNode,
) {
  return h(
    Dialog,
    null,
    h(
      DialogTrigger,
      { asChild: true },
      h("button", { "data-testid": `${id}-trigger` }, `Open ${id}`),
    ),
    h(
      DialogContent,
      { ...contentProps, ...{ "data-testid": id } },
      h(DialogTitle, null, id),
      h(DialogDescription, null, `Controls for ${id}`),
      h("input", { "data-testid": `${id}-input`, "aria-label": id }),
      children,
      h(
        DialogPrimitive.Close,
        { asChild: true },
        h("button", { "data-testid": `${id}-close` }, "Close"),
      ),
    ),
  );
}
async function click(id: string) {
  await act(async () => {
    const target = element(id);
    target.focus();
    target.click();
  });
}
async function open(
  contentProps: React.ComponentProps<typeof DialogContent> = {},
) {
  await act(async () =>
    root.render(
      h(
        "div",
        null,
        h("button", { "data-testid": "new-owner" }, "New task"),
        h("button", { "data-testid": "custom-owner" }, "Custom destination"),
        dialog("dialog", contentProps),
      ),
    ),
  );
  await click("dialog-trigger");
  await act(async () => vi.runOnlyPendingTimers());
  expect(document.activeElement).toBe(element("dialog-input"));
}
async function finishClose() {
  // Drive real Radix FocusScope cleanup, without mocking its callbacks or
  // matching its implementation's function text.
  await act(async () => vi.runOnlyPendingTimers());
}
it("restores the original trigger after ordinary dialog close", async () => {
  await open();
  await click("dialog-close");
  expect(document.querySelector('[data-testid="dialog"]')).toBeNull();
  expect(document.activeElement).toBe(document.body);
  await finishClose();
  expect(document.activeElement).toBe(element("dialog-trigger"));
});
it("preserves a newer connected focus owner when deferred close finishes", async () => {
  await open();
  await click("dialog-close");
  element("new-owner").focus();
  await finishClose();
  expect(document.activeElement).toBe(element("new-owner"));
});
it.each(["body", "documentElement", "focus-guard"])(
  "still restores the trigger when only %s owns focus",
  async (owner) => {
    await open();
    await click("dialog-close");
    let guard: HTMLSpanElement | undefined;
    if (owner === "documentElement") {
      document.documentElement.tabIndex = -1;
      document.documentElement.focus();
    } else if (owner === "focus-guard") {
      guard = document.createElement("span");
      guard.dataset.radixFocusGuard = "";
      guard.tabIndex = 0;
      document.body.append(guard);
      guard.focus();
    }
    try {
      await finishClose();
      expect(document.activeElement).toBe(element("dialog-trigger"));
    } finally {
      guard?.remove();
    }
  },
);
it("lets a custom close handler choose focus before the default guard", async () => {
  const onCloseAutoFocus = vi.fn((event: Event) => {
    expect(event.defaultPrevented).toBe(false);
    expect(document.activeElement).toBe(element("new-owner"));
    element("custom-owner").focus();
    event.preventDefault();
  });
  await open({ onCloseAutoFocus });
  await click("dialog-close");
  element("new-owner").focus();
  await finishClose();
  expect(onCloseAutoFocus).toHaveBeenCalledOnce();
  expect(document.activeElement).toBe(element("custom-owner"));
});
it("keeps ordinary restoration when a custom handler does not prevent it", async () => {
  const onCloseAutoFocus = vi.fn();
  await open({ onCloseAutoFocus });
  await click("dialog-close");
  await finishClose();
  expect(onCloseAutoFocus).toHaveBeenCalledOnce();
  expect(document.activeElement).toBe(element("dialog-trigger"));
});
it("restores a nested dialog's opener without closing its parent", async () => {
  await act(async () => root.render(dialog("parent", {}, dialog("child"))));
  await click("parent-trigger");
  await finishClose();
  await click("child-trigger");
  await finishClose();
  expect(document.activeElement).toBe(element("child-input"));
  await click("child-close");
  await finishClose();
  expect(document.activeElement).toBe(element("child-trigger"));
  expect(element("parent").getAttribute("data-state")).toBe("open");
  await click("parent-close");
  await finishClose();
  expect(document.activeElement).toBe(element("parent-trigger"));
});
