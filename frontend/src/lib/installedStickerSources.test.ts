import { notifySyncInvalidation } from "./syncInvalidation";
import { encodeStickerMediaName } from "./stickerMedia";
import { RECOVERY_REFRESH_MS } from "./recoveryRefresh";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { CustomizationPack } from "./pack";
const mocks = vi.hoisted(() => ({
  current: true,
  account: "account",
  register: vi.fn(),
  pending: vi.fn(),
  state: {
    loaded: true,
    scope: { owner: "owner", account: "account" },
    packs: [] as CustomizationPack[],
    contentToken: (_id: string) => JSON.stringify(mocks.state.packs),
    readPack: vi.fn(async (id: string) =>
      mocks.state.packs.find((pack) => pack.id === id),
    ),
  },
  listeners: new Set<() => void>(),
}));
vi.mock("./api/chat", () => ({
  chat: {
    registerStickerSource: mocks.register,
    pendingStickerSources: mocks.pending,
  },
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ owner: "owner", current: () => mocks.current }),
  currentRuntimeAccountId: () => mocks.account,
}));
vi.mock("@/store/packs", () => ({
  usePacks: {
    getState: () => mocks.state,
    subscribe: (listener: () => void) => {
      mocks.listeners.add(listener);
      return () => mocks.listeners.delete(listener);
    },
  },
}));
import { startInstalledStickerSources } from "./installedStickerSources";
const pack = (url = "data:image/png;base64,AQID"): CustomizationPack => ({
  format: 1,
  id: "cats",
  version: "1.0.0",
  name: "Cats",
  kind: "sticker",
  stickers: [{ id: "wave", label: "Wave", fallback: "👋", url }],
});
const changed = () => mocks.listeners.forEach((listener) => listener());
const settle = async () => {
  for (let i = 0; i < 10; i++) await Promise.resolve();
};
let stop: (() => void) | undefined;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("window", globalThis);
  mocks.current = true;
  mocks.account = "account";
  mocks.pending.mockReset();
  mocks.pending.mockResolvedValue({ items: [], next_cursor: null });
  mocks.state.readPack.mockClear();
  mocks.register.mockReset();
  mocks.register.mockResolvedValue(undefined);
  mocks.state.scope = { owner: "owner", account: "account" };
  mocks.state.loaded = true;
  mocks.state.packs = [pack()];
  mocks.listeners.clear();
});
afterEach(() => {
  stop?.();
  stop = undefined;
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
it("does no artwork work without native demand, including repair and pack changes", async () => {
  stop = startInstalledStickerSources("owner");
  await settle();
  mocks.state.packs = [pack("data:image/png;base64,BAUG")];
  changed();
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS);
  expect(mocks.state.readPack).not.toHaveBeenCalled();
  expect(mocks.register).not.toHaveBeenCalled();
});

const demand = (n = 1, id = "wave") => ({
  file_conv: n.toString(16).padStart(64, "0"),
  name: encodeStickerMediaName(`pack:cats:${id}`, "👋", "png", "Wave"),
  mime: "image/png",
  size: 3,
});
const wake = () => notifySyncInvalidation({ owner: "owner", kind: "files" });
const page = (items = [demand()]) => ({ items, next_cursor: null });
it("reads only the exact demanded asset among many installed assets", async () => {
  const p = pack();
  if (p.kind === "sticker")
    for (let i = 0; i < 200; i++)
      p.stickers.push({
        id: `other${i}`,
        label: "Other",
        fallback: "🙂",
        url: "invalid",
      });
  mocks.state.packs = [p, { ...pack(), id: "other" }];
  mocks.pending.mockResolvedValue(page());
  stop = startInstalledStickerSources("owner");
  await settle();
  expect(mocks.state.readPack).toHaveBeenCalledOnce();
  expect(mocks.state.readPack.mock.calls[0][0]).toBe("cats");
  expect(mocks.register).toHaveBeenCalledOnce();
  expect(mocks.register.mock.calls[0][3]).toEqual([1, 2, 3]);
});
it("progresses through empty filtered pages and more than 64 rows serially", async () => {
  mocks.pending
    .mockResolvedValueOnce({ items: [], next_cursor: demand(1).file_conv })
    .mockResolvedValueOnce({
      items: Array.from({ length: 64 }, (_, i) => demand(i + 2)),
      next_cursor: demand(65).file_conv,
    })
    .mockResolvedValueOnce(page([demand(66)]));
  stop = startInstalledStickerSources("owner");
  await settle();
  await vi.advanceTimersByTimeAsync(1);
  await settle();
  await vi.advanceTimersByTimeAsync(1);
  await settle();
  expect(mocks.pending).toHaveBeenCalledTimes(3);
  expect(mocks.register).toHaveBeenCalledTimes(65);
});
it("ignores unrelated state emissions and sync invalidations", async () => {
  stop = startInstalledStickerSources("owner");
  await settle();
  changed();
  for (const kind of ["roster", "delivery", "contacts"] as const)
    notifySyncInvalidation({ owner: "owner", kind });
  notifySyncInvalidation({ owner: "other", kind: "files" });
  notifySyncInvalidation({
    owner: "owner",
    kind: "state",
    scope: "sync_status",
  });
  await settle();
  expect(mocks.pending).toHaveBeenCalledOnce();
  notifySyncInvalidation({ owner: "owner", kind: "state" });
  await settle();
  expect(mocks.pending).toHaveBeenCalledTimes(2);
});
it.each(["stop", "account", "content"])(
  "retires a held pack read on %s change",
  async (change) => {
    let resolve!: (p: CustomizationPack) => void;
    mocks.state.readPack.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    mocks.pending.mockResolvedValueOnce(page());
    stop = startInstalledStickerSources("owner");
    await settle();
    if (change === "stop") stop();
    if (change === "account") mocks.account = "other";
    if (change === "content") {
      mocks.state.packs = [pack("data:image/png;base64,BAUG")];
      changed();
    }
    resolve(pack());
    await settle();
    expect(mocks.register).not.toHaveBeenCalled();
  },
);
it("keeps actual registration serial while dirty, then finishes suffix before fresh head", async () => {
  let resolve!: () => void;
  mocks.register.mockImplementationOnce(
    () =>
      new Promise<void>((done) => {
        resolve = done;
      }),
  );
  mocks.pending
    .mockResolvedValueOnce({
      items: [demand()],
      next_cursor: demand().file_conv,
    })
    .mockResolvedValueOnce(page([demand(2)]))
    .mockResolvedValueOnce(page());
  stop = startInstalledStickerSources("owner");
  await settle();
  wake();
  wake();
  await settle();
  expect(mocks.pending).toHaveBeenCalledOnce();
  resolve();
  await settle();
  await vi.advanceTimersByTimeAsync(1);
  await settle();
  expect(mocks.pending.mock.calls.map((call) => call[2])).toEqual([
    null,
    demand().file_conv,
    null,
  ]);
  expect(mocks.register).toHaveBeenCalledTimes(3);
});
it("stops held registration before the next demand after account retirement", async () => {
  let resolve!: () => void;
  mocks.register.mockImplementationOnce(
    () =>
      new Promise<void>((done) => {
        resolve = done;
      }),
  );
  mocks.pending.mockResolvedValue(page([demand(), demand(2)]));
  stop = startInstalledStickerSources("owner");
  await settle();
  mocks.account = "other";
  resolve();
  await settle();
  expect(mocks.register).toHaveBeenCalledOnce();
});
it.each(["io", "not-started", "authorization"])(
  "retries transient %s failures only on a wake",
  async (kind) => {
    mocks.pending.mockResolvedValue(page());
    mocks.register.mockRejectedValueOnce({ kind, message: "retry" });
    stop = startInstalledStickerSources("owner");
    await settle();
    expect(mocks.register).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS);
    expect(mocks.register).toHaveBeenCalledTimes(2);
  },
);
it("parks native invalid artwork by token and retries replacement", async () => {
  mocks.pending.mockResolvedValue(page());
  mocks.register.mockRejectedValue({
    kind: "invalid-input",
    message: "bad image",
  });
  stop = startInstalledStickerSources("owner");
  await settle();
  wake();
  await settle();
  expect(mocks.register).toHaveBeenCalledOnce();
  mocks.state.packs = [pack("data:image/png;base64,BAUG")];
  changed();
  await settle();
  expect(mocks.register).toHaveBeenCalledTimes(2);
});
it("does not remember successful registration across native eviction demand", async () => {
  mocks.pending.mockResolvedValue(page());
  stop = startInstalledStickerSources("owner");
  await settle();
  wake();
  await settle();
  expect(mocks.register).toHaveBeenCalledTimes(2);
});
it("rejects oversized and nonadvancing native pages", async () => {
  mocks.pending
    .mockResolvedValueOnce({ items: [], next_cursor: demand().file_conv })
    .mockResolvedValueOnce({ items: [], next_cursor: demand().file_conv });
  stop = startInstalledStickerSources("owner");
  await settle();
  await vi.advanceTimersByTimeAsync(1);
  await settle();
  expect(mocks.pending).toHaveBeenCalledTimes(2);
  wake();
  mocks.pending.mockResolvedValue(
    page(Array.from({ length: 65 }, (_, i) => demand(i + 1))),
  );
  await settle();
  expect(mocks.register).not.toHaveBeenCalled();
});
it("bounds permanent rejection receipts to 128 demand identities", async () => {
  let offset = 0;
  mocks.pending.mockImplementation(async (_owner, _account, cursor) => {
    if (cursor === null) offset = 0;
    const items = Array.from({ length: Math.min(32, 129 - offset) }, (_, i) =>
      demand(offset + i + 1),
    );
    offset += items.length;
    return {
      items,
      next_cursor: offset < 129 ? demand(offset).file_conv : null,
    };
  });
  mocks.register.mockRejectedValue({
    kind: "invalid-input",
    message: "bad image",
  });
  stop = startInstalledStickerSources("owner");
  await settle();
  for (let i = 0; i < 4; i++) {
    await vi.advanceTimersByTimeAsync(1);
    await settle();
  }
  expect(mocks.register).toHaveBeenCalledTimes(129);
  mocks.pending.mockResolvedValue(page());
  wake();
  await settle();
  expect(mocks.register).toHaveBeenCalledTimes(130);
});
it("parks malformed snapshots and permits corrected pack content", async () => {
  mocks.state.packs = [pack("invalid")];
  mocks.pending.mockResolvedValue(page());
  stop = startInstalledStickerSources("owner");
  await settle();
  wake();
  await settle();
  expect(mocks.state.readPack).toHaveBeenCalledOnce();
  expect(mocks.register).not.toHaveBeenCalled();
  mocks.state.packs = [pack()];
  changed();
  await settle();
  expect(mocks.register).toHaveBeenCalledOnce();
});
it("retries failed demand and pack reads on later meaningful events", async () => {
  mocks.pending
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValue(page());
  mocks.state.readPack.mockRejectedValueOnce(new Error("busy"));
  stop = startInstalledStickerSources("owner");
  await settle();
  expect(mocks.pending).toHaveBeenCalledOnce();
  wake();
  await settle();
  expect(mocks.register).not.toHaveBeenCalled();
  wake();
  await settle();
  expect(mocks.register).toHaveBeenCalledOnce();
});
it("does not consume another account's pack scope", async () => {
  mocks.state.scope.account = "other";
  mocks.pending.mockResolvedValue(page());
  stop = startInstalledStickerSources("owner");
  await settle();
  expect(mocks.pending).not.toHaveBeenCalled();
  expect(mocks.state.readPack).not.toHaveBeenCalled();
});
it("bounds self-wakes while retaining late demand for sparse repair", async () => {
  mocks.pending.mockResolvedValue(page());
  let registrations = 0;
  mocks.register.mockImplementation(async () => {
    if (++registrations === 2)
      mocks.pending.mockResolvedValue(page([demand(3)]));
    wake();
  });
  stop = startInstalledStickerSources("owner");
  await settle();
  expect(mocks.register).toHaveBeenCalledTimes(2);
  mocks.pending.mockResolvedValue(page([demand(3)]));
  mocks.register.mockResolvedValue(undefined);
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS);
  await settle();
  expect(mocks.pending).toHaveBeenCalledTimes(3);
  expect(mocks.register).toHaveBeenCalledTimes(3);
});
