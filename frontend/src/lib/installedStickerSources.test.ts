import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { CustomizationPack } from "./pack";
const mocks = vi.hoisted(() => ({
  current: true,
  register: vi.fn(),
  state: {
    loaded: true,
    scope: { owner: "owner", account: "account" },
    packs: [] as CustomizationPack[],
  },
  listeners: new Set<() => void>(),
}));
vi.mock("./api/chat", () => ({
  chat: { registerStickerSource: mocks.register },
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ owner: "owner", current: () => mocks.current }),
  currentRuntimeAccountId: () => "account",
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
it("registers exact installed bytes once per runtime and refreshes replaced artwork", async () => {
  stop = startInstalledStickerSources("owner");
  await settle();
  expect(mocks.register).toHaveBeenCalledOnce();
  expect(mocks.register.mock.calls[0].slice(0, 2)).toEqual([
    "owner",
    "account",
  ]);
  expect(mocks.register.mock.calls[0][2]).toMatch(/^mt-sticker-v1-.*\.png$/);
  expect(mocks.register.mock.calls[0][3]).toEqual([1, 2, 3]);
  changed();
  await settle();
  expect(mocks.register).toHaveBeenCalledOnce();
  mocks.state.packs = [pack("data:image/png;base64,BAUG")];
  changed();
  await settle();
  expect(mocks.register).toHaveBeenCalledTimes(2);
  expect(mocks.register.mock.calls[1][3]).toEqual([4, 5, 6]);
});
it("does not register another account's packs or work after runtime retirement", async () => {
  mocks.state.scope = { owner: "owner", account: "other" };
  stop = startInstalledStickerSources("owner");
  await settle();
  expect(mocks.register).not.toHaveBeenCalled();
  mocks.state.scope.account = "account";
  mocks.current = false;
  changed();
  await settle();
  expect(mocks.register).not.toHaveBeenCalled();
});
it("stops a serial snapshot batch when its account changes during an IPC call", async () => {
  let resolve!: () => void;
  mocks.register.mockReturnValueOnce(
    new Promise<void>((done) => {
      resolve = done;
    }),
  );
  const p = pack();
  if (p.kind === "sticker")
    p.stickers.push({
      id: "smile",
      label: "Smile",
      fallback: "🙂",
      url: "data:image/png;base64,BAUG",
    });
  mocks.state.packs = [p];
  stop = startInstalledStickerSources("owner");
  await settle();
  expect(mocks.register).toHaveBeenCalledOnce();
  mocks.current = false;
  resolve();
  await settle();
  expect(mocks.register).toHaveBeenCalledOnce();
});
it("retries failed local registration without re-registering successful artwork", async () => {
  mocks.register.mockRejectedValueOnce(new Error("runtime starting"));
  stop = startInstalledStickerSources("owner");
  await settle();
  await vi.advanceTimersByTimeAsync(10_000);
  await settle();
  expect(mocks.register).toHaveBeenCalledTimes(2);
  await vi.advanceTimersByTimeAsync(10_000);
  await settle();
  expect(mocks.register).toHaveBeenCalledTimes(2);
});
