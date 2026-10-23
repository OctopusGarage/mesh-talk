import { afterEach, expect, it, vi } from "vitest";
import { createDraftStore } from "./drafts";

afterEach(() => vi.useRealTimers());
function fixture() {
  vi.useFakeTimers();
  const api = {
    load: vi
      .fn()
      .mockResolvedValue({ session: "1", drafts: { "account:a": "saved" } }),
    save: vi.fn().mockResolvedValue(undefined),
  };
  const store = createDraftStore(api);
  return { api, store };
}
it("coalesces typing until ten seconds idle and restores exact text", async () => {
  const { api, store } = fixture();
  await store.getState().start("alice", 1);
  expect(store.getState().texts["account:a"]).toBe("saved");
  store.getState().edit("alice", 1, "account:a", "你\n");
  await vi.advanceTimersByTimeAsync(9000);
  store.getState().edit("alice", 1, "account:a", "你\n好");
  await vi.advanceTimersByTimeAsync(9000);
  expect(api.save).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1000);
  expect(api.save).toHaveBeenCalledExactlyOnceWith("alice", "1", {
    "account:a": "你\n好",
  });
});
it("waits for ten seconds of inactivity even during continuous typing", async () => {
  const { api, store } = fixture();
  await store.getState().start("alice", 1);
  for (let i = 0; i < 20; i++) {
    store.getState().edit("alice", 1, "account:a", String(i));
    await vi.advanceTimersByTimeAsync(500);
  }
  expect(api.save).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(9500);
  expect(api.save).toHaveBeenCalledTimes(1);
});
it("flushes clearing and keeps independent conversations", async () => {
  const { api, store } = fixture();
  await store.getState().start("alice", 1);
  store.getState().edit("alice", 1, "channel:b", "group");
  store.getState().edit("alice", 1, "account:a", "");
  await store.getState().flush();
  expect(api.save).toHaveBeenLastCalledWith("alice", "1", {
    "channel:b": "group",
  });
  await vi.advanceTimersByTimeAsync(10000);
  expect(api.save).toHaveBeenCalledTimes(1);
});
it("rejects retired composer edits and delayed owner loads", async () => {
  const { api, store } = fixture();
  let resolve!: (value: {
    session: string;
    drafts: Record<string, string>;
  }) => void;
  api.load.mockImplementationOnce(
    () =>
      new Promise((yes) => {
        resolve = yes;
      }),
  );
  const old = store.getState().start("alice", 1);
  await store.getState().start("bob", 2);
  resolve({ session: "1", drafts: { "account:secret": "private" } });
  await old;
  store.getState().edit("alice", 1, "account:secret", "stale");
  expect(store.getState().texts).toEqual({ "account:a": "saved" });
  expect(api.save).not.toHaveBeenCalled();
});
it("serializes saves and retains edits made while a write is pending", async () => {
  const { api, store } = fixture();
  let resolve!: () => void;
  api.save.mockImplementationOnce(
    () =>
      new Promise<void>((yes) => {
        resolve = yes;
      }),
  );
  await store.getState().start("alice", 1);
  store.getState().edit("alice", 1, "account:a", "first");
  const first = store.getState().flush();
  await vi.advanceTimersByTimeAsync(0);
  store.getState().edit("alice", 1, "account:a", "second");
  const second = store.getState().flush();
  expect(api.save).toHaveBeenCalledTimes(1);
  resolve();
  await Promise.all([first, second]);
  expect(api.save).toHaveBeenLastCalledWith("alice", "1", {
    "account:a": "second",
  });
});
it("keeps text on failed persistence and allows an explicit retry", async () => {
  const { api, store } = fixture();
  await store.getState().start("alice", 1);
  api.save.mockRejectedValueOnce(new Error("disk full"));
  store.getState().edit("alice", 1, "account:a", "keep");
  await store.getState().flush();
  expect(store.getState().error).toBe("save");
  expect(store.getState().texts["account:a"]).toBe("keep");
  await store.getState().flush();
  expect(store.getState().error).toBeNull();
});

it("does not hold a replacement session behind a retired pending write", async () => {
  const { api, store } = fixture();
  let resolve!: () => void;
  api.save.mockImplementationOnce(
    () =>
      new Promise<void>((yes) => {
        resolve = yes;
      }),
  );
  await store.getState().start("alice", 1);
  store.getState().edit("alice", 1, "account:a", "old");
  const retired = store.getState().flush();
  await vi.advanceTimersByTimeAsync(0);
  await store.getState().start("bob", 2);
  store.getState().edit("bob", 2, "account:a", "new");
  const current = store.getState().flush();
  await vi.advanceTimersByTimeAsync(0);
  expect(api.save).toHaveBeenCalledTimes(2);
  resolve();
  await Promise.all([retired, current]);
  expect(store.getState().texts["account:a"]).toBe("new");
});
