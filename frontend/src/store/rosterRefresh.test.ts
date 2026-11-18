import { expect, it, vi } from "vitest";
import {
  createRosterRefresh,
  createContactHistoryRefresh,
} from "./rosterRefresh";

it("coalesces busy invalidations to the latest input", async () => {
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const observed: (number | undefined)[] = [];
  const refresh = createRosterRefresh<number>({
    capture: () => ({ current: () => true }),
    refresh: async (_, input) => {
      observed.push(input);
      if (observed.length === 1) await held;
    },
  });
  const pending = refresh(1);
  for (let i = 2; i <= 1000; i++) void refresh(i);
  expect(observed).toEqual([1]);
  release();
  await pending;
  expect(observed).toEqual([1, 1000]);
});

it("bounds distinct sender hints to 256 and one trailing timer, then admits fresh devices", async () => {
  vi.useFakeTimers();
  const refresh = vi.fn(async () => {});
  const hints = createContactHistoryRefresh(refresh);
  try {
    for (let i = 0; i < 256; i++) {
      hints.observe("device", String(i));
      await Promise.resolve();
    }
    expect(refresh).toHaveBeenCalledTimes(256);
    expect(vi.getTimerCount()).toBe(0);
    for (let i = 256; i < 1024; i++) hints.observe("device", String(i));
    expect(refresh).toHaveBeenCalledTimes(256);
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(2000);
    expect(refresh).toHaveBeenCalledTimes(257);
    expect(vi.getTimerCount()).toBe(0);
    hints.observe("device", "new-device");
    expect(refresh).toHaveBeenCalledTimes(258);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(refresh).toHaveBeenCalledTimes(258);
  } finally {
    hints.reset();
    vi.useRealTimers();
  }
});

it("rechecks distinct accounts immediately, including one sharing a device hint's ID", async () => {
  vi.useFakeTimers();
  const refresh = vi.fn(async () => {});
  const hints = createContactHistoryRefresh(refresh);
  try {
    hints.observe("device", "same-id");
    hints.observe("account", "same-id");
    hints.observe("account", "second-id");
    expect(refresh).toHaveBeenCalledTimes(3);
    hints.observe("account", "same-id");
    await vi.advanceTimersByTimeAsync(100);
    expect(refresh).toHaveBeenCalledTimes(3);
    await vi.advanceTimersByTimeAsync(1900);
    expect(refresh).toHaveBeenCalledTimes(4);
    expect(vi.getTimerCount()).toBe(0);
  } finally {
    hints.reset();
    vi.useRealTimers();
  }
});

it("retires deferred sender work and a held old read across reset", async () => {
  vi.useFakeTimers();
  let finish!: () => void;
  const refresh = vi
    .fn<() => Promise<void>>()
    .mockResolvedValue(undefined)
    .mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
  const hints = createContactHistoryRefresh(refresh);
  try {
    hints.observe("device", "same-device");
    hints.observe("device", "same-device");
    expect(vi.getTimerCount()).toBe(0);
    hints.reset();
    hints.observe("device", "same-device");
    await Promise.resolve();
    hints.observe("device", "same-device");
    expect(vi.getTimerCount()).toBe(1);
    finish();
    await Promise.resolve();
    expect(vi.getTimerCount()).toBe(1);
    hints.reset();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(refresh).toHaveBeenCalledTimes(2);
    expect(vi.getTimerCount()).toBe(0);
  } finally {
    hints.reset();
    vi.useRealTimers();
  }
});
