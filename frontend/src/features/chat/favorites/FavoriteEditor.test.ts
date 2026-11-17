// @vitest-environment jsdom
import { act, createElement, Profiler } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
const owner = vi.hoisted(() => ({ current: true }));
beforeEach(() => {
  owner.current = true;
});
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ owner: "alice", current: () => owner.current }),
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
it("describes a legacy sticker without its pack instead of treating its emoji fallback as text", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(
        createElement(FavoriteEditor, {
          draft: {
            ...draft,
            messages: [
              { ...draft.messages[0], text: "🙂", sticker: "unknown" },
            ],
          },
          onClose: () => {},
        }),
      ),
    );
    expect(document.body.textContent).toContain("[Sticker]");
  } finally {
    await act(async () => root.unmount());
  }
});
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

it("does not commit one-second renders for a mounted non-expiring editor", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date(2026, 0, 2, 12));
  const root = createRoot(document.createElement("div"));
  const commits = vi.fn();
  try {
    await act(async () =>
      root.render(
        createElement(
          Profiler,
          { id: "editor", onRender: commits },
          createElement(FavoriteEditor, { draft, onClose: () => {} }),
        ),
      ),
    );
    await act(async () => vi.advanceTimersByTime(0));
    commits.mockClear();
    await act(async () => vi.advanceTimersByTime(5_000));
    expect(commits).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
  }
});
it("withholds editor content exactly at its fractional-second expiry", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(10_000);
  const root = createRoot(document.createElement("div"));
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
    await act(async () => vi.advanceTimersByTime(499));
    expect(document.body.textContent).toContain("short-lived secret");
    await act(async () => vi.advanceTimersByTime(1));
    expect(document.body.textContent).not.toContain("short-lived secret");
  } finally {
    await act(async () => root.unmount());
  }
});
it("never resurrects expired editor text after wall-clock rollback", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(10_000);
  const root = createRoot(document.createElement("div"));
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
    await act(async () => vi.advanceTimersByTime(1_000));
    expect(document.body.textContent).not.toContain("short-lived secret");
    vi.setSystemTime(8_000);
    await act(async () => vi.advanceTimersByTime(1_000));
    expect(document.body.textContent).not.toContain("short-lived secret");
  } finally {
    await act(async () => root.unmount());
  }
});
it("updates the local calendar label at midnight without a one-second delay", async () => {
  vi.useFakeTimers();
  const wall = new Date(2026, 0, 2, 23, 59, 59, 500).getTime();
  vi.setSystemTime(wall);
  const root = createRoot(document.createElement("div"));
  try {
    await act(async () =>
      root.render(
        createElement(FavoriteEditor, {
          draft: {
            ...draft,
            messages: [{ ...draft.messages[0], wallClock: wall }],
          },
          onClose: () => {},
        }),
      ),
    );
    const before = document.querySelector("time")?.textContent;
    await act(async () => vi.advanceTimersByTime(500));
    expect(document.querySelector("time")?.textContent).not.toBe(before);
  } finally {
    await act(async () => root.unmount());
  }
});
it("refreshes editor expiry on focus while refusing retired-owner clock publications", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(10_000);
  const root = createRoot(document.createElement("div"));
  const commits = vi.fn();
  try {
    await act(async () =>
      root.render(
        createElement(
          Profiler,
          { id: "editor", onRender: commits },
          createElement(FavoriteEditor, {
            draft: {
              ...draft,
              messages: [{ ...draft.messages[0], expiresAtMs: 20_000 }],
            },
            onClose: () => {},
          }),
        ),
      ),
    );
    vi.setSystemTime(21_000);
    await act(async () => window.dispatchEvent(new Event("focus")));
    expect(document.body.textContent).not.toContain("short-lived secret");
    owner.current = false;
    commits.mockClear();
    await act(async () => vi.advanceTimersByTime(1_000));
    expect(commits).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
  }
});
