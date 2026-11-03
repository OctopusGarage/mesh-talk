// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";

const state = vi.hoisted(() => ({
  auth: { user: { id: "alice" }, generation: 1 },
  chat: {
    myAccountId: "root-a",
    active: { kind: "account", id: "bob", name: "Bob" },
    accounts: [],
    favorites: {},
    ready: true,
  },
  policy: {
    owner: "alice",
    contacts: { bob: { account_id: "bob", name: "Bob" } },
    loaded: true,
    loading: false,
    busy: false,
    error: null,
    change: vi.fn(),
    clearSaveError: vi.fn(),
  },
  setChat: vi.fn(),
}));
vi.mock("@/store/auth", () => ({
  useAuth: Object.assign(
    (select: (s: typeof state.auth) => unknown) => select(state.auth),
    { getState: () => state.auth },
  ),
}));
vi.mock("@/store/chat", () => ({
  useChat: Object.assign(
    (select: (s: typeof state.chat) => unknown) => select(state.chat),
    { getState: () => state.chat, setState: state.setChat },
  ),
}));
vi.mock("@/store/contactPolicy", () => ({
  useContactPolicy: Object.assign(
    (select: (s: typeof state.policy) => unknown) => select(state.policy),
    { getState: () => state.policy },
  ),
}));
vi.mock("@/components/identity", () => ({ IdentityGlyph: () => null }));
import {
  HiddenContactsDialog,
  HideContactDialog,
} from "./HiddenContactsDialog";

let root: Root;
let host: HTMLDivElement;
const close = vi.fn();
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  state.chat.myAccountId = "root-a";
  state.policy.change.mockReset().mockResolvedValue(true);
  state.setChat.mockReset();
  close.mockReset();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
const button = (id: string) =>
  document.querySelector<HTMLButtonElement>('[data-testid="' + id + '"]')!;
const renderHide = () =>
  act(async () => {
    root.render(
      createElement(HideContactDialog, {
        contact: { account_id: "bob", name: "Bob" },
        onClose: close,
      }),
    );
  });

it("passes the captured account when hiding a contact", async () => {
  await renderHide();
  await act(async () => button("hide-contact-confirm").click());
  expect(state.policy.change).toHaveBeenCalledWith(
    "alice",
    "bob",
    true,
    "Bob",
    "root-a",
  );
});
it("passes the captured account when restoring a contact", async () => {
  await act(async () => root.render(createElement(HiddenContactsDialog)));
  await act(async () => button("manage-hidden-contacts").click());
  await act(async () => button("restore-contact-bob").click());
  expect(state.policy.change).toHaveBeenCalledWith(
    "alice",
    "bob",
    false,
    "Bob",
    "root-a",
  );
});
it("does not close a replacement account's chat after an old hide completes", async () => {
  let resolve!: (ok: boolean) => void;
  state.policy.change.mockImplementationOnce(
    () =>
      new Promise<boolean>((finish) => {
        resolve = finish;
      }),
  );
  await renderHide();
  await act(async () => button("hide-contact-confirm").click());
  state.chat.myAccountId = "root-b";
  await act(async () => resolve(true));
  expect(state.setChat).not.toHaveBeenCalled();
  expect(close).not.toHaveBeenCalled();
});
