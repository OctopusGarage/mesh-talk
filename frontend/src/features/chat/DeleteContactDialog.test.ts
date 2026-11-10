// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";

const state = vi.hoisted(() => ({
  online: false,
  ready: true,
  deleteContact: vi.fn(),
}));
vi.mock("@/store/chat", () => ({
  useChat: Object.assign(
    (selector: (s: typeof state) => unknown) => selector(state),
    { getState: () => state },
  ),
}));
vi.mock("@/store/presence", () => ({
  usePresenceFor: () => ({ online: state.online }),
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ current: () => true }),
}));
import { DeleteContactDialog } from "./DeleteContactDialog";

let root: Root;
let container: HTMLDivElement;
const onClose = vi.fn();
const render = () =>
  act(async () => {
    root.render(
      createElement(DeleteContactDialog, {
        account: "account",
        name: "Offline contact",
        onClose,
      }),
    );
  });
const button = (id: string) =>
  document.querySelector<HTMLButtonElement>(`[data-testid="${id}"]`)!;
beforeEach(() => {
  (
    globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  state.online = false;
  state.ready = true;
  state.deleteContact.mockReset().mockResolvedValue(true);
  onClose.mockReset();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

it("focuses cancel and only deletes after explicit confirmation", async () => {
  await render();
  expect(document.activeElement).toBe(button("delete-contact-cancel"));
  expect(document.body.textContent).toContain("linked devices");
  expect(state.deleteContact).not.toHaveBeenCalled();
  await act(async () => button("delete-contact-confirm").click());
  expect(state.deleteContact).toHaveBeenCalledWith("account", false);
  expect(onClose).toHaveBeenCalledOnce();
});
it("cancels without deleting", async () => {
  await render();
  await act(async () => button("delete-contact-cancel").click());
  expect(state.deleteContact).not.toHaveBeenCalled();
  expect(onClose).toHaveBeenCalledOnce();
});
it("allows deleting an online contact and keeps history by default", async () => {
  state.online = true;
  await render();
  expect(button("delete-contact-confirm").disabled).toBe(false);
  expect(
    document.querySelector<HTMLInputElement>(
      '[data-testid="delete-contact-history"]',
    )?.checked,
  ).toBe(false);
  await act(async () => button("delete-contact-confirm").click());
  expect(state.deleteContact).toHaveBeenCalledWith("account", false);
});
it("deletes history only when the checkbox is selected", async () => {
  await render();
  await act(async () =>
    document
      .querySelector<HTMLInputElement>(
        '[data-testid="delete-contact-history"]',
      )!
      .click(),
  );
  await act(async () => button("delete-contact-confirm").click());
  expect(state.deleteContact).toHaveBeenCalledWith("account", true);
});
it("keeps the dialog open and offers retry when deletion fails", async () => {
  state.deleteContact.mockResolvedValue(false);
  await render();
  await act(async () => button("delete-contact-confirm").click());
  expect(onClose).not.toHaveBeenCalled();
  expect(document.querySelector('[role="alert"]')?.textContent).toContain(
    "Try again",
  );
  expect(button("delete-contact-confirm").disabled).toBe(false);
});
