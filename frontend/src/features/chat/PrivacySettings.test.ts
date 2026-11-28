// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { PrivacySettings } from "./PrivacySettings";
const fixture = vi.hoisted(() => ({
  auth: { user: { id: "alice" } },
  privacy: {
    owner: "alice",
    loaded: true,
    loading: false,
    busy: false,
    error: null as string | null,
    snapshot: {
      invisible: false,
      contacts_only: false,
      allowed_accounts: [
        { id: "b".repeat(32), name: "Bob", source: "Initiated" },
      ],
      blocked_accounts: ["c".repeat(32)],
    },
    load: vi.fn(),
    setInvisible: vi.fn(),
    setAllowed: vi.fn(),
    setPersonal: vi.fn(),
  },
  chat: {
    myAccountId: "a".repeat(32),
    accounts: [
      { account_id: "a".repeat(32), names: ["Me"] },
      { account_id: "d".repeat(32), names: ["Dana"] },
    ],
  },
}));
vi.mock("react-i18next", async (original) => ({
  ...(await original<typeof import("react-i18next")>()),
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/store/auth", () => ({
  useAuth: Object.assign(
    (s: (v: typeof fixture.auth) => unknown) => s(fixture.auth),
    { getState: () => fixture.auth },
  ),
}));
vi.mock("@/store/chat", () => ({
  useChat: (s: (v: typeof fixture.chat) => unknown) => s(fixture.chat),
}));
vi.mock("@/store/privacy", () => ({
  usePrivacy: Object.assign(
    (s: (v: typeof fixture.privacy) => unknown) => s(fixture.privacy),
    { getState: () => fixture.privacy },
  ),
}));
let host: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  fixture.privacy.busy = false;
  fixture.privacy.error = null;
  fixture.privacy.load.mockReset().mockResolvedValue(undefined);
  fixture.privacy.setInvisible.mockReset().mockResolvedValue(true);
  fixture.privacy.setAllowed.mockReset().mockResolvedValue(true);
  fixture.privacy.setPersonal.mockReset().mockResolvedValue(true);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
async function render() {
  await act(async () => root.render(createElement(PrivacySettings)));
}
async function click(selector: string) {
  await act(async () =>
    document.querySelector<HTMLButtonElement>(selector)!.click(),
  );
}
it("loads owner policy and applies personal contact and block controls", async () => {
  await render();
  expect(fixture.privacy.load).toHaveBeenCalledWith("alice");
  await click("#contacts-only-switch");
  expect(fixture.privacy.setPersonal).toHaveBeenCalledWith("alice", null, true);
  await click('[data-testid="manage-privacy"]');
  expect(document.body.textContent).toContain("Dana");
  expect(document.body.textContent).toContain("privacy.initiated");
  await click('[data-testid="privacy-allow-' + "d".repeat(32) + '"]');
  expect(fixture.privacy.setAllowed).toHaveBeenCalledWith(
    "alice",
    "d".repeat(32),
    true,
  );
  const unblock = [
    ...document.querySelectorAll<HTMLButtonElement>("button"),
  ].find((b) => b.textContent === "privacy.unblock")!;
  await act(async () => unblock.click());
  expect(fixture.privacy.setPersonal).toHaveBeenCalledWith(
    "alice",
    "c".repeat(32),
    false,
  );
  const block = [
    ...document.querySelectorAll<HTMLButtonElement>("button"),
  ].find((b) => b.textContent === "privacy.block")!;
  await act(async () => block.click());
  expect(fixture.privacy.setPersonal).toHaveBeenLastCalledWith(
    "alice",
    "b".repeat(32),
    true,
  );
});
it("confirms invisibility and account revocation before saving", async () => {
  await render();
  await click("#invisible-switch");
  expect(fixture.privacy.setInvisible).not.toHaveBeenCalled();
  await click('[data-testid="invisible-confirm"]');
  expect(fixture.privacy.setInvisible).toHaveBeenCalledWith("alice", true);
  await click('[data-testid="manage-privacy"]');
  await click('[data-testid="privacy-revoke-' + "b".repeat(32) + '"]');
  expect(fixture.privacy.setAllowed).not.toHaveBeenCalled();
  await click('[data-testid="privacy-revoke-confirm"]');
  expect(fixture.privacy.setAllowed).toHaveBeenCalledWith(
    "alice",
    "b".repeat(32),
    false,
  );
});
it("offers a policy retry after a load error", async () => {
  fixture.privacy.error = "load";
  await render();
  expect(document.querySelector('[role="alert"]')?.textContent).toBe(
    "privacy.loadError",
  );
  const retry = [
    ...document.querySelectorAll<HTMLButtonElement>("button"),
  ].find((b) => b.textContent === "privacy.retry")!;
  await act(async () => retry.click());
  expect(fixture.privacy.load).toHaveBeenCalledTimes(2);
});

it("cancels pending privacy confirmations and updates the contact search", async () => {
  await render();
  await click("#invisible-switch");
  await click('[data-testid="invisible-cancel"]');
  expect(fixture.privacy.setInvisible).not.toHaveBeenCalled();
  await click('[data-testid="manage-privacy"]');
  await click('[data-testid="privacy-revoke-' + "b".repeat(32) + '"]');
  await click('[data-testid="privacy-revoke-cancel"]');
  expect(fixture.privacy.setAllowed).not.toHaveBeenCalled();
  await click('[data-testid="privacy-revoke-' + "b".repeat(32) + '"]');
  await act(async () =>
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  expect(fixture.privacy.setAllowed).not.toHaveBeenCalled();
  const input = document.querySelector<HTMLInputElement>("#privacy-search")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, "nobody matches");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  expect(document.querySelectorAll('[role="listitem"]')).toHaveLength(0);
});

it("merges blocked contacts already present in the roster and supports legacy policies", async () => {
  const blocked = fixture.privacy.snapshot.blocked_accounts;
  fixture.privacy.snapshot.blocked_accounts = ["d".repeat(32)];
  try {
    await render();
    await click('[data-testid="manage-privacy"]');
    expect(document.querySelectorAll('[role="listitem"]')).toHaveLength(2);
    await act(async () => root.render(null));
    Reflect.deleteProperty(fixture.privacy.snapshot, "blocked_accounts");
    await render();
    await click('[data-testid="manage-privacy"]');
    expect(document.querySelectorAll('[role="listitem"]')).toHaveLength(2);
    expect(document.body.textContent).not.toContain("privacy.unblock");
  } finally {
    fixture.privacy.snapshot.blocked_accounts = blocked;
  }
});
