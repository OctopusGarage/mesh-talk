import { expect, it, vi } from "vitest";
import { createMessageFavoritesStore } from "./messageFavorites";
import type { MessageFavorite } from "@/lib/api/messageFavorites";
const source = {
  kind: "account" as const,
  target: "alice",
  message_ids: ["a"],
};
const metadata = { title: "", description: "Useful", notes: { a: "key step" } };
const item = {
  id: "f",
  source,
  metadata,
  source_label: "Alice",
  messages: [],
  saved_at_ms: 1,
  revision: 1,
  device_id: "device",
} as MessageFavorite;
function setup() {
  let current = true;
  const api = {
    list: vi.fn(async () => ({ items: [item], has_more: false })),
    save: vi.fn(async () => item),
    edit: vi.fn(async () => item),
    remove: vi.fn(async () => {}),
  };
  const store = createMessageFavoritesStore(api, () => ({
    owner: "owner",
    current: () => current,
  }));
  return {
    api,
    store,
    retire: () => {
      current = false;
      store.getState().reset();
    },
  };
}
it("discards a list that completes after logout", async () => {
  const { api, store, retire } = setup();
  let resolve!: (value: {
    items: MessageFavorite[];
    has_more: boolean;
  }) => void;
  api.list.mockImplementation(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  const pending = store.getState().load("");
  retire();
  resolve({ items: [item], has_more: false });
  await pending;
  expect(store.getState().items).toEqual([]);
});
it("older searches cannot overwrite a newer query", async () => {
  const { api, store } = setup();
  let resolve!: (value: {
    items: MessageFavorite[];
    has_more: boolean;
  }) => void;
  api.list.mockImplementationOnce(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  const older = store.getState().load("old");
  await store.getState().load("notes");
  resolve({ items: [], has_more: false });
  await older;
  expect(store.getState().query).toBe("notes");
  expect(store.getState().items).toEqual([item]);
});
it("passes only references and personal metadata to a durable save", async () => {
  const { api, store } = setup();
  await store.getState().save(source, metadata);
  expect(api.save).toHaveBeenCalledWith("owner", source, metadata);
});
it.each(["load", "save"] as const)(
  "%s recovers from a stalled favorites read without losing durable acceptance",
  async (operation) => {
    vi.useFakeTimers();
    const { api, store } = setup();
    let finish!: (value: {
      items: MessageFavorite[];
      has_more: boolean;
    }) => void;
    api.list.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    const completed = vi.fn();
    const pending = (
      operation === "load"
        ? store.getState().load("")
        : store.getState().save(source, metadata)
    ).then(completed);
    try {
      await vi.advanceTimersByTimeAsync(10_001);
      expect(completed).toHaveBeenCalledOnce();
      if (operation === "save") expect(completed).toHaveBeenCalledWith(item);
      expect(store.getState().loading).toBe(false);
      expect(store.getState().error).toMatch(/timed out/i);
      await store.getState().load("retry");
      finish({ items: [], has_more: false });
      await pending;
      await vi.advanceTimersByTimeAsync(0);
      expect(store.getState().items).toEqual([item]);
    } finally {
      finish({ items: [], has_more: false });
      await pending;
      vi.useRealTimers();
    }
  },
);
it("preserves collection state after failed removal", async () => {
  const { api, store } = setup();
  await store.getState().load("");
  api.remove.mockRejectedValueOnce(new Error("disk full"));
  await expect(store.getState().remove(item)).rejects.toThrow("disk full");
  expect(store.getState().items).toEqual([item]);
});
it("refresh keeps every loaded page rather than dropping older results", async () => {
  const { api, store } = setup();
  const all = Array.from({ length: 45 }, (_, index) => ({
    ...item,
    id: `favorite-${index}`,
  }));
  api.list.mockImplementation(async (...args: unknown[]) => {
    const offset = Number(args[2] ?? 0),
      limit = Number(args[3] ?? 20);
    return {
      items: all.slice(offset, offset + limit),
      has_more: offset + limit < all.length,
    };
  });
  await store.getState().load("");
  await store.getState().load("", true);
  expect(store.getState().items).toHaveLength(40);
  await store.getState().refresh();
  expect(store.getState().items).toHaveLength(40);
  expect(store.getState().items[39].id).toBe("favorite-39");
});
