// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useAuth } from "@/store/auth";
import type { AccountBindingChange } from "@/lib/api/privacy";
import { AccountBindingRecovery } from "./AccountBindingRecovery";

const api = vi.hoisted(() => ({
  bindingChanges: vi.fn(),
  acceptBindingChange: vi.fn(),
}));
vi.mock("@/lib/api/privacy", () => ({ privacy: api }));
vi.mock("@/store/chat", () => ({
  useChat: (select: (state: unknown) => unknown) =>
    select({ runEpoch: 1, identityEpoch: 1, refreshRoster: async () => {} }),
}));
const change: AccountBindingChange = {
  device_id: "contact-device-fingerprint",
  previous_account_id: "previous-account",
  current_account_id: "kept-account",
};
let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  useAuth.setState({
    user: {
      id: "recipient-owner",
      username: "recipient",
      display_name: "Recipient",
    },
    generation: 1,
  });
  api.bindingChanges
    .mockReset()
    .mockResolvedValue({ owner: "recipient-owner", changes: [change] });
  api.acceptBindingChange
    .mockReset()
    .mockResolvedValue({ owner: "recipient-owner" });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
async function render(accountId = "previous-account") {
  await act(async () =>
    root.render(createElement(AccountBindingRecovery, { accountId })),
  );
}
async function accept() {
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="accept-account-change"]',
      )!
      .click(),
  );
}

it("only inspects on open, shows exact bindings, and accepts only after explicit action", async () => {
  await render();
  expect(api.bindingChanges).toHaveBeenCalledWith(
    "recipient-owner",
    "previous-account",
  );
  expect(api.acceptBindingChange).not.toHaveBeenCalled();
  expect(host.textContent).toContain(change.previous_account_id);
  expect(host.textContent).toContain(change.current_account_id);
  expect(host.textContent).toContain(change.device_id);
  await accept();
  expect(api.acceptBindingChange).toHaveBeenCalledExactlyOnceWith(
    "recipient-owner",
    change,
  );
  expect(
    host.querySelector('[data-testid="accept-account-change"]'),
  ).toBeNull();
  expect(host.querySelector('[role="status"]')).not.toBeNull();
});

it("keeps unrelated devices awaiting separate explicit review", async () => {
  const other = { ...change, device_id: "second-contact-device" };
  api.bindingChanges.mockResolvedValue({
    owner: "recipient-owner",
    changes: [change, other],
  });
  await render();
  await accept();
  expect(api.acceptBindingChange).toHaveBeenCalledExactlyOnceWith(
    "recipient-owner",
    change,
  );
  expect(host.textContent).toContain(other.device_id);
  expect(
    host.querySelectorAll('[data-testid="accept-account-change"]'),
  ).toHaveLength(1);
});

it("discards inspection from a previous contact and owner generation", async () => {
  let finish!: (result: unknown) => void;
  api.bindingChanges
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValueOnce({ owner: "recipient-owner", changes: [] });
  await render();
  await render("another-contact");
  await act(async () =>
    finish({ owner: "recipient-owner", changes: [change] }),
  );
  expect(host.textContent).not.toContain(change.device_id);
  expect(api.acceptBindingChange).not.toHaveBeenCalled();
});

it("withholds acceptance success when a different contact replaces the dialog", async () => {
  let finish!: (value: { owner: string }) => void;
  api.acceptBindingChange.mockImplementationOnce(
    () =>
      new Promise<{ owner: string }>((resolve) => {
        finish = resolve;
      }),
  );
  await render();
  await accept();
  api.bindingChanges.mockResolvedValue({
    owner: "recipient-owner",
    changes: [],
  });
  await render("another-contact");
  await act(async () => finish({ owner: "recipient-owner" }));
  expect(host.querySelector('[role="status"]')).toBeNull();
  expect(
    host.querySelector('[data-testid="accept-account-change"]'),
  ).toBeNull();
});

it("shows failed acceptance and lets the recipient refresh the proposal", async () => {
  api.acceptBindingChange.mockRejectedValueOnce({
    kind: "invalid-input",
    message: "Account change is no longer current",
  });
  await render();
  await accept();
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "This account change is no longer current",
  );
  expect(
    host.querySelector<HTMLButtonElement>(
      '[data-testid="accept-account-change"]',
    )?.disabled,
  ).toBe(false);
});

it("opens only the explicitly accepted current account as a separate conversation", async () => {
  const onOpenAccount = vi.fn();
  await act(async () =>
    root.render(
      createElement(AccountBindingRecovery, {
        accountId: change.previous_account_id,
        onOpenAccount,
      }),
    ),
  );
  await accept();
  expect(onOpenAccount).not.toHaveBeenCalled();
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="open-current-account"]')!
      .click(),
  );
  expect(onOpenAccount).toHaveBeenCalledExactlyOnceWith(
    change.current_account_id,
  );
});
