import { afterEach, expect, it, vi } from "vitest";
import { createBadgeSync } from "./badgeSync";

afterEach(() => vi.useRealTimers());
it("retries failures with the latest count and clears diagnostic state after success", async () => {
  vi.useFakeTimers();
  const write = vi
    .fn()
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValue(undefined);
  const status = vi.fn();
  const sync = createBadgeSync({ write, status, current: () => true });
  sync.update(7);
  await vi.advanceTimersByTimeAsync(0);
  expect(status).toHaveBeenLastCalledWith("offline");
  sync.update(0);
  await vi.advanceTimersByTimeAsync(1000);
  expect(write.mock.calls.map(([count]) => count)).toEqual([7, 0]);
  expect(status).toHaveBeenLastCalledWith(null);
  sync.stop();
});
it("serializes changes and coalesces rapid updates to the newest count", async () => {
  let finish!: () => void;
  const write = vi
    .fn()
    .mockImplementationOnce(
      () =>
        new Promise<void>((r) => {
          finish = r;
        }),
    )
    .mockResolvedValue(undefined);
  const sync = createBadgeSync({ write, status: vi.fn(), current: () => true });
  sync.update(8);
  sync.update(4);
  sync.update(0);
  expect(write).toHaveBeenCalledTimes(1);
  finish();
  await vi.waitFor(() =>
    expect(write.mock.calls.map(([count]) => count)).toEqual([8, 0]),
  );
  sync.stop();
});
it("times out stalled writes, retries, and ignores retired work", async () => {
  vi.useFakeTimers();
  let current = true;
  const write = vi.fn().mockImplementation(() => new Promise(() => {}));
  const status = vi.fn();
  const sync = createBadgeSync({ write, status, current: () => current });
  sync.update(2);
  await vi.advanceTimersByTimeAsync(5000);
  expect(status).toHaveBeenLastCalledWith("Badge update timed out");
  await vi.advanceTimersByTimeAsync(1000);
  expect(write).toHaveBeenCalledTimes(2);
  current = false;
  await vi.advanceTimersByTimeAsync(60_000);
  expect(write).toHaveBeenCalledTimes(2);
  sync.stop();
});
it("force-refreshes an unchanged count and cancels scheduled retries on stop", async () => {
  vi.useFakeTimers();
  const write = vi.fn().mockResolvedValue(undefined);
  const sync = createBadgeSync({ write, status: vi.fn(), current: () => true });
  sync.update(1);
  await vi.advanceTimersByTimeAsync(0);
  sync.update(1);
  expect(write).toHaveBeenCalledTimes(1);
  sync.update(1, true);
  await vi.advanceTimersByTimeAsync(0);
  expect(write).toHaveBeenCalledTimes(2);
  write.mockRejectedValue(new Error("failed"));
  sync.update(0);
  await vi.advanceTimersByTimeAsync(0);
  sync.stop();
  await vi.advanceTimersByTimeAsync(60_000);
  expect(write).toHaveBeenCalledTimes(3);
});

it("bounds unresolved native calls even when every timed-out write never settles", async () => {
  vi.useFakeTimers();
  const write = vi.fn(() => new Promise<void>(() => {}));
  const sync = createBadgeSync({ write, status: vi.fn(), current: () => true });
  sync.update(1);
  await vi.advanceTimersByTimeAsync(300_000);
  expect(write).toHaveBeenCalledTimes(4);
  sync.stop();
});
