// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { TrustCardPanel } from "./TrustCardPanel";
import { useAuth } from "@/store/auth";
import type { TrustCardView } from "@/lib/api/trustCards";
const mocks = vi.hoisted(() => ({ get: vi.fn(), publish: vi.fn() }));
vi.mock("@/lib/api/trustCards", () => ({ trustCards: mocks }));
let root: Root;
let host: HTMLDivElement;
function card(account: string): TrustCardView {
  return {
    account_id: account,
    revision: 1,
    display_name: account,
    bio: "",
    created_at_ms: null,
    avatar: null,
    introductions: [],
    witnesses: [],
    signed_json: "{}",
  };
}
beforeEach(() => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  mocks.get.mockReset();
  mocks.publish.mockReset();
  useAuth.setState({
    user: {
      id: "owner",
      username: "tester",
      display_name: "Tester",
    },
    generation: 1,
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
it("discards a card response when its contact has changed", async () => {
  let finish!: (value: TrustCardView) => void;
  mocks.get
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValueOnce(card("bob"));
  await act(async () =>
    root.render(createElement(TrustCardPanel, { accountId: "alice" })),
  );
  await act(async () =>
    root.render(createElement(TrustCardPanel, { accountId: "bob" })),
  );
  await act(async () => finish(card("alice")));
  expect(
    host.querySelector('[data-testid="signed-card-details"]')?.textContent,
  ).toContain("bob");
  expect(
    host.querySelector('[data-testid="signed-card-details"]')?.textContent,
  ).not.toContain("alice");
});
it("discards a response after the local session generation retires", async () => {
  let finish!: (value: TrustCardView) => void;
  mocks.get.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  await act(async () =>
    root.render(createElement(TrustCardPanel, { accountId: "alice" })),
  );
  await act(async () => {
    useAuth.setState({ generation: 2 });
    finish(card("alice"));
  });
  expect(host.querySelector('[data-testid="signed-card-details"]')).toBeNull();
});
it("preserves the exact known creation timestamp when publishing profile edits", async () => {
  const timestamp = Date.UTC(2026, 0, 1, 12, 34, 56, 789);
  mocks.get.mockResolvedValue({ ...card("self"), created_at_ms: timestamp });
  mocks.publish.mockResolvedValue(undefined);
  await act(async () =>
    root.render(
      createElement(TrustCardPanel, { accountId: "self", own: true }),
    ),
  );
  expect(
    host.querySelector<HTMLInputElement>('input[type="date"]')?.disabled,
  ).toBe(true);
  const publish = Array.from(host.querySelectorAll("button")).find(
    (button) => button.textContent === "Publish signed card",
  );
  await act(async () => publish!.click());
  expect(mocks.publish).toHaveBeenCalledWith("owner", "self", "", timestamp);
});
