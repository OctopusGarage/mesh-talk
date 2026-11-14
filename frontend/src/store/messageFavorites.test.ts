import { expect, it, vi } from "vitest";
import { createMessageFavoritesStore } from "./messageFavorites";
import { readWithDeadline } from "@/lib/readDeadline";
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

it.each(["resolve", "reject"] as const)(
  "coalesces background refresh until the actual timed-out list %ss",
  async (completion) => {
    vi.useFakeTimers();
    const { api, store } = setup();
    let finish!: (page: {
      items: MessageFavorite[];
      has_more: boolean;
    }) => void;
    let fail!: (error: Error) => void;
    api.list.mockImplementationOnce(
      () =>
        new Promise((yes, no) => {
          finish = yes;
          fail = no;
        }),
    );
    const fresh = { ...item, id: "fresh" };
    api.list.mockResolvedValue({ items: [fresh], has_more: false });
    const observed = store.getState().refresh();
    try {
      await vi.advanceTimersByTimeAsync(10_001);
      await observed;
      expect(store.getState().loading).toBe(false);
      expect(store.getState().error).toMatch(/timed out/i);
      for (let i = 0; i < 3; i++) await store.getState().refresh();
      expect(api.list).toHaveBeenCalledOnce();
      if (completion === "resolve") finish({ items: [item], has_more: false });
      else fail(new Error("late physical list failure"));
      await vi.advanceTimersByTimeAsync(0);
      expect(api.list).toHaveBeenCalledTimes(2);
      expect(store.getState().items).toEqual([fresh]);
      expect(store.getState().error).toBeNull();
      await vi.advanceTimersByTimeAsync(30_000);
      expect(api.list).toHaveBeenCalledTimes(2);
    } finally {
      finish({ items: [], has_more: false });
      await observed;
      await vi.advanceTimersByTimeAsync(0);
      vi.useRealTimers();
    }
  },
);

it("does not let an actual pending old query block explicit search or its background refresh", async () => {
  vi.useFakeTimers();
  const { api, store } = setup();
  let finish!: (page: { items: MessageFavorite[]; has_more: boolean }) => void;
  api.list.mockImplementationOnce(
    () =>
      new Promise((yes) => {
        finish = yes;
      }),
  );
  const observed = store.getState().refresh();
  try {
    await vi.advanceTimersByTimeAsync(10_001);
    await observed;
    await store.getState().load("new query");
    expect(api.list).toHaveBeenCalledTimes(2);
    expect(api.list).toHaveBeenLastCalledWith("owner", "new query", 0, 20);
    await store.getState().refresh();
    expect(api.list).toHaveBeenCalledTimes(3);
    finish({ items: [{ ...item, id: "old" }], has_more: false });
    await vi.advanceTimersByTimeAsync(0);
    expect(store.getState().query).toBe("new query");
    expect(store.getState().items).toEqual([item]);
    expect(api.list).toHaveBeenCalledTimes(3);
  } finally {
    finish({ items: [], has_more: false });
    await observed;
    vi.useRealTimers();
  }
});

it("retires queued refresh intent on reset without blocking a replacement runtime owner", async () => {
  vi.useFakeTimers();
  const { api } = setup();
  let runtime = 0;
  const store = createMessageFavoritesStore(api, () => {
    const captured = runtime;
    return { owner: `owner-${captured}`, current: () => captured === runtime };
  });
  let finish!: (page: { items: MessageFavorite[]; has_more: boolean }) => void;
  api.list.mockImplementationOnce(
    () =>
      new Promise((yes) => {
        finish = yes;
      }),
  );
  const observed = store.getState().refresh();
  let queued: Promise<void> | undefined;
  try {
    await vi.advanceTimersByTimeAsync(10_001);
    await observed;
    queued = store.getState().refresh();
    runtime++;
    store.getState().reset();
    api.list.mockClear();
    await store.getState().refresh();
    expect(api.list).toHaveBeenCalledOnce();
    expect(api.list).toHaveBeenLastCalledWith("owner-1", "", 0, 20);
    finish({ items: [{ ...item, id: "retired" }], has_more: false });
    await queued;
    await vi.advanceTimersByTimeAsync(0);
    expect(store.getState().items).toEqual([item]);
    expect(api.list).toHaveBeenCalledOnce();
  } finally {
    finish({ items: [], has_more: false });
    await observed;
    await queued;
    vi.useRealTimers();
  }
});

it("releases refresh ownership after synchronous list failure and does not loop after success", async () => {
  const { api, store } = setup();
  api.list.mockImplementationOnce(() => {
    throw new Error("synchronous list failure");
  });
  await store.getState().refresh();
  expect(store.getState().error).toContain("synchronous list failure");
  await store.getState().refresh();
  expect(store.getState().items).toEqual([item]);
  expect(store.getState().loading).toBe(false);
  expect(api.list).toHaveBeenCalledTimes(2);
  await Promise.resolve();
  expect(api.list).toHaveBeenCalledTimes(2);
});

it("finishes the list observer watchdog after a durable mutation queues its refresh", async () => {
  vi.useFakeTimers();
  const { api, store } = setup();
  let finish!: (page: { items: MessageFavorite[]; has_more: boolean }) => void;
  api.list.mockImplementationOnce(
    () =>
      new Promise((yes) => {
        finish = yes;
      }),
  );
  const fresh = { ...item, id: "after-mutation" };
  api.list.mockResolvedValue({ items: [fresh], has_more: false });
  const observed = store.getState().refresh();
  try {
    const accepted = await store.getState().save(source, metadata);
    expect(accepted).toBe(item);
    expect(api.save).toHaveBeenCalledOnce();
    expect(api.list).toHaveBeenCalledOnce();
    expect(store.getState().loading).toBe(true);
    await vi.advanceTimersByTimeAsync(10_001);
    await observed;
    expect(store.getState().loading).toBe(false);
    expect(store.getState().error).toMatch(/timed out/i);
    finish({ items: [{ ...item, id: "before-mutation" }], has_more: false });
    await vi.advanceTimersByTimeAsync(0);
    expect(api.list).toHaveBeenCalledTimes(2);
    expect(api.save).toHaveBeenCalledOnce();
    expect(store.getState().items).toEqual([fresh]);
    expect(store.getState().error).toBeNull();
  } finally {
    finish({ items: [], has_more: false });
    await observed;
    await vi.advanceTimersByTimeAsync(0);
    vi.useRealTimers();
  }
});

it("admits the queued refresh after the settled physical list releases its global slot", async () => {
  vi.useFakeTimers();
  const { api, store } = setup();
  let finish!: (page: { items: MessageFavorite[]; has_more: boolean }) => void;
  api.list.mockImplementationOnce(
    () =>
      new Promise((yes) => {
        finish = yes;
      }),
  );
  const fresh = { ...item, id: "after-slot-release" };
  api.list.mockResolvedValue({ items: [fresh], has_more: false });
  const observed = store.getState().refresh();
  const releaseOthers: Array<() => void> = [];
  const otherObservers = Array.from({ length: 63 }, () =>
    readWithDeadline(
      () =>
        new Promise<void>((yes) => {
          releaseOthers.push(yes);
        }),
    ).catch(() => {}),
  );
  try {
    expect(releaseOthers).toHaveLength(63);
    expect(api.list).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(10_001);
    await observed;
    await Promise.all(otherObservers);
    await store.getState().refresh();
    expect(api.list).toHaveBeenCalledOnce();
    finish({ items: [item], has_more: false });
    await vi.advanceTimersByTimeAsync(0);
    expect(api.list).toHaveBeenCalledTimes(2);
    expect(store.getState().items).toEqual([fresh]);
    expect(store.getState().error).toBeNull();
  } finally {
    finish({ items: [], has_more: false });
    for (const release of releaseOthers) release();
    await observed;
    await Promise.all(otherObservers);
    await vi.advanceTimersByTimeAsync(0);
    vi.useRealTimers();
  }
});
