import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { MediaObjectUrlCache } from "./mediaObjectUrlCache";

let cache: MediaObjectUrlCache;
const createUrl = vi.fn();
const revokeUrl = vi.fn();
beforeEach(() => {
  vi.useFakeTimers();
  let next = 0;
  createUrl.mockReset().mockImplementation(() => `blob:media-${++next}`);
  revokeUrl.mockReset();
  vi.spyOn(URL, "createObjectURL").mockImplementation(createUrl);
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(revokeUrl);
  cache = new MediaObjectUrlCache({ maxEntries: 2, maxBytes: 8, idleMs: 1000 });
});
afterEach(() => {
  cache.clear();
  vi.restoreAllMocks();
  vi.useRealTimers();
});
const current = () => true;
const blob = (size = 4) => new Blob([new Uint8Array(size)]);

it("shares a read and URL between simultaneous consumers and remounts", async () => {
  const read = vi.fn(async () => blob());
  const first = cache.acquire("image", read, current);
  const second = cache.acquire("image", read, current);
  expect(await first.result).toBe(await second.result);
  expect(read).toHaveBeenCalledOnce();
  first.release();
  second.release();
  expect(cache.peek("image")).toBe("blob:media-1");
  const remount = cache.acquire("image", read, current);
  expect(await remount.result).toBe("blob:media-1");
  expect(read).toHaveBeenCalledOnce();
  remount.release();
});
it("expires idle URLs but never expires an actively displayed URL", async () => {
  const first = cache.acquire("image", async () => blob(), current);
  await first.result;
  await vi.advanceTimersByTimeAsync(2000);
  expect(revokeUrl).not.toHaveBeenCalled();
  first.release();
  await vi.advanceTimersByTimeAsync(999);
  expect(cache.peek("image")).not.toBeNull();
  await vi.advanceTimersByTimeAsync(1);
  expect(cache.peek("image")).toBeNull();
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:media-1");
});
it("bounds retained bytes and evicts the least recently used idle URL", async () => {
  const first = cache.acquire("first", async () => blob(), current);
  await first.result;
  first.release();
  const second = cache.acquire("second", async () => blob(), current);
  await second.result;
  second.release();
  const reused = cache.acquire("first", async () => blob(), current);
  await reused.result;
  reused.release();
  const third = cache.acquire("third", async () => blob(), current);
  await third.result;
  third.release();
  expect(cache.peek("first")).not.toBeNull();
  expect(cache.peek("second")).toBeNull();
  expect(cache.peek("third")).not.toBeNull();
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:media-2");
});
it("enforces the idle entry cap even for zero-length blobs", async () => {
  for (const key of ["first", "second", "third"]) {
    const entry = cache.acquire(key, async () => blob(0), current);
    await entry.result;
    entry.release();
  }
  expect(cache.peek("first")).toBeNull();
  expect(cache.peek("second")).not.toBeNull();
  expect(cache.peek("third")).not.toBeNull();
});
it("keeps active readers valid while dropping over-budget idle media", async () => {
  const active = cache.acquire("active", async () => blob(10), current);
  await active.result;
  const idle = cache.acquire("idle", async () => blob(), current);
  await idle.result;
  idle.release();
  expect(cache.peek("active")).toBe("blob:media-1");
  active.release();
  expect(cache.peek("active")).toBeNull();
  expect(cache.peek("idle")).toBe("blob:media-2");
});
it("revokes stale owners including idle entries exactly once", async () => {
  let alive = true;
  const entry = cache.acquire(
    "image",
    async () => blob(),
    () => alive,
  );
  await entry.result;
  alive = false;
  cache.invalidateStale();
  expect(cache.peek("image")).toBeNull();
  entry.release();
  cache.clear();
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:media-1");
});
it("cancels pending work once the last consumer leaves, even across a remount", async () => {
  let finish!: (blob: Blob) => void;
  let admittedCurrent!: () => boolean;
  const old = cache.acquire(
    "image",
    (isCurrent) => {
      admittedCurrent = isCurrent;
      return new Promise<Blob>((resolve) => {
        finish = resolve;
      });
    },
    current,
  );
  await Promise.resolve();
  old.release();
  expect(admittedCurrent()).toBe(false);
  const next = cache.acquire("image", async () => blob(), current);
  const nextUrl = await next.result;
  finish(blob());
  expect(await old.result).toBeNull();
  expect(cache.peek("image")).toBe(nextUrl);
  expect(createUrl).toHaveBeenCalledOnce();
  next.release();
});
it("does not cache failures, and allows a later successful read", async () => {
  const failed = cache.acquire(
    "image",
    async () => {
      throw new Error("missing");
    },
    current,
  );
  await expect(failed.result).rejects.toThrow("missing");
  failed.release();
  const retried = cache.acquire("image", async () => blob(), current);
  expect(await retried.result).toBe("blob:media-1");
  retried.release();
});
it("does not publish a late read after owner invalidation", async () => {
  let finish!: (blob: Blob) => void;
  let alive = true;
  const entry = cache.acquire(
    "image",
    () =>
      new Promise<Blob>((resolve) => {
        finish = resolve;
      }),
    () => alive,
  );
  await Promise.resolve();
  alive = false;
  cache.invalidateStale();
  finish(blob());
  expect(await entry.result).toBeNull();
  expect(createUrl).not.toHaveBeenCalled();
  entry.release();
});

it("enforces the combined byte budget before reaching the idle entry cap", async () => {
  const first = cache.acquire("first", async () => blob(6), current);
  await first.result;
  first.release();
  const second = cache.acquire("second", async () => blob(6), current);
  await second.result;
  second.release();
  expect(cache.peek("first")).toBeNull();
  expect(cache.peek("second")).toBe("blob:media-2");
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:media-1");
});

it("keeps a shared pending read admitted when only one consumer leaves", async () => {
  let finish!: (blob: Blob) => void;
  let admittedCurrent!: () => boolean;
  const read = vi.fn((isCurrent: () => boolean) => {
    admittedCurrent = isCurrent;
    return new Promise<Blob>((resolve) => {
      finish = resolve;
    });
  });
  const first = cache.acquire("image", read, current);
  const second = cache.acquire("image", read, current);
  await Promise.resolve();
  first.release();
  expect(admittedCurrent()).toBe(true);
  finish(blob());
  expect(await second.result).toBe("blob:media-1");
  expect(read).toHaveBeenCalledOnce();
  second.release();
});
