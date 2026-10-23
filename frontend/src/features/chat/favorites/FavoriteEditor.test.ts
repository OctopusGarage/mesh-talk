// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ owner: "alice", current: () => true }),
}));
vi.mock("@/store/packs", () => ({
  usePacks: (select: (state: { packs: never[] }) => unknown) =>
    select({ packs: [] }),
}));
vi.mock("@/store/messageFavorites", () => ({
  useMessageFavorites: { getState: () => ({ edit: vi.fn(), save: vi.fn() }) },
}));
import { FavoriteEditor, type FavoriteDraft } from "./FavoriteEditor";
const draft: FavoriteDraft = {
  source: { kind: "account", target: "alice", message_ids: ["m1"] },
  messages: [
    { id: "m1", who: "Alice", text: "short-lived secret", wallClock: 1 },
  ],
};
afterEach(() => vi.useRealTimers());
it("withholds original content when its explicit deadline passes in an open editor", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(10_000);
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(
        createElement(FavoriteEditor, {
          draft: {
            ...draft,
            messages: [{ ...draft.messages[0], expiresAtMs: 10_500 }],
          },
          onClose: () => {},
        }),
      ),
    );
    expect(document.body.textContent).toContain("short-lived secret");
    await act(async () => vi.advanceTimersByTime(1000));
    expect(document.body.textContent).not.toContain("short-lived secret");
    expect(document.body.textContent).toContain(
      "Message content is no longer available",
    );
  } finally {
    await act(async () => root.unmount());
    container.remove();
  }
});
it("prefills overall and per-message remarks when editing an existing exact selection", async () => {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(
        createElement(FavoriteEditor, {
          draft: {
            ...draft,
            existing: {
              id: "f",
              source: draft.source,
              source_label: "Alice",
              messages: [],
              saved_at_ms: 1,
              revision: 2,
              device_id: "d",
              metadata: {
                title: "Runbook",
                description: "Existing explanation",
                notes: { m1: "Critical step" },
              },
            },
          },
          onClose: () => {},
        }),
      ),
    );
    expect(
      (
        document.querySelector(
          '[data-testid="favorite-title"]',
        ) as HTMLInputElement
      ).value,
    ).toBe("Runbook");
    expect(
      (
        document.querySelector(
          '[data-testid="favorite-description"]',
        ) as HTMLTextAreaElement
      ).value,
    ).toBe("Existing explanation");
    expect(
      (
        document.querySelector(
          '[data-testid="favorite-note-0"]',
        ) as HTMLTextAreaElement
      ).value,
    ).toBe("Critical step");
  } finally {
    await act(async () => root.unmount());
    container.remove();
  }
});
