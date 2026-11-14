// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useSerialPoll } from "./useSerialPoll";
import {
  notifySyncInvalidation,
  setPushStateSupport,
} from "@/lib/syncInvalidation";
import { readWithDeadline } from "@/lib/readDeadline";

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.spyOn(document, "hidden", "get").mockReturnValue(false);
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  setPushStateSupport(null);
});

it("keeps push-backed recovery sparse rather than reading unchanged state every thirty seconds", async () => {
  vi.useFakeTimers();
  setPushStateSupport("alice");
  const request = vi.fn().mockResolvedValue(1);
  const root = createRoot(document.createElement("div"));
  function Probe() {
    useSerialPoll(
      true,
      5000,
      request,
      () => {},
      () => {},
      "state",
    );
    return null;
  }
  try {
    await act(async () => root.render(createElement(Probe)));
    await act(async () => vi.advanceTimersByTime(299_000));
    expect(request).toHaveBeenCalledTimes(1);
    await act(async () => vi.advanceTimersByTime(1000));
    expect(request).toHaveBeenCalledTimes(2);
  } finally {
    await act(async () => root.unmount());
  }
});

it("retains one actual native request after its UI watchdog expires", async () => {
  vi.useFakeTimers();
  let finish!: (value: number) => void;
  const pending = new Promise<number>((resolve) => {
    finish = resolve;
  });
  const request = vi
    .fn()
    .mockImplementationOnce(() => pending)
    .mockResolvedValue(7);
  const received = vi.fn();
  const failed = vi.fn();
  const root = createRoot(document.createElement("div"));
  function Probe() {
    useSerialPoll(true, 1000, request, received, failed);
    return null;
  }
  try {
    await act(async () => root.render(createElement(Probe)));
    for (let index = 0; index < 20; index++)
      await act(async () => vi.advanceTimersByTime(1000));
    expect(failed).toHaveBeenCalledOnce();
    expect(request).toHaveBeenCalledTimes(1);
    await act(async () => finish(1));
    expect(received).not.toHaveBeenCalled();
    await act(async () => vi.advanceTimersByTime(1000));
    expect(request).toHaveBeenCalledTimes(2);
    expect(received).toHaveBeenCalledExactlyOnceWith(7);
  } finally {
    finish(1);
    await act(async () => root.unmount());
  }
});

it("skips overlapping polls and ignores a response after close", async () => {
  vi.useFakeTimers();
  let resolveFirst!: (value: number) => void;
  const first = new Promise<number>((resolve) => {
    resolveFirst = resolve;
  });
  let resolveSecond!: (value: number) => void;
  const second = new Promise<number>((resolve) => {
    resolveSecond = resolve;
  });
  const request = vi
    .fn()
    .mockImplementationOnce(() => first)
    .mockImplementationOnce(() => second);
  const received: number[] = [];
  const element = document.createElement("div");
  const root = createRoot(element);
  function Probe({ open }: { open: boolean }) {
    useSerialPoll(
      open,
      100,
      request,
      (value: number) => received.push(value),
      () => {},
    );
    return null;
  }

  await act(async () => root.render(createElement(Probe, { open: true })));
  await act(async () => vi.advanceTimersByTime(300));
  expect(request).toHaveBeenCalledTimes(1);
  await act(async () => resolveFirst(1));
  expect(received).toEqual([1]);

  await act(async () => vi.advanceTimersByTime(100));
  expect(request).toHaveBeenCalledTimes(2);
  await act(async () => root.render(createElement(Probe, { open: false })));
  await act(async () => resolveSecond(2));
  expect(received).toEqual([1]);
  await act(async () => root.unmount());
});

it("preserves qualified status wakes for existing default state subscribers", async () => {
  const { notifySyncInvalidation } = await import("@/lib/syncInvalidation");
  const request = vi.fn().mockResolvedValue(1);
  const root = createRoot(document.createElement("div"));
  function Probe() {
    useSerialPoll(
      true,
      30000,
      request,
      () => {},
      () => {},
      "state",
    );
    return null;
  }
  try {
    await act(async () => root.render(createElement(Probe)));
    expect(request).toHaveBeenCalledTimes(1);
    await act(async () =>
      notifySyncInvalidation({
        owner: "alice",
        kind: "state",
        scope: "sync_status",
      }),
    );
    expect(request).toHaveBeenCalledTimes(2);
  } finally {
    await act(async () => root.unmount());
  }
});

it("subscribes before reading and coalesces changes that arrive during the actual request", async () => {
  const request = vi
    .fn()
    .mockImplementationOnce(() => {
      for (let index = 0; index < 10; index++)
        notifySyncInvalidation({ owner: "alice", kind: "state" });
      return Promise.resolve(1);
    })
    .mockResolvedValue(2);
  const root = createRoot(document.createElement("div"));
  function Probe() {
    useSerialPoll(
      true,
      5000,
      request,
      () => {},
      () => {},
      "state",
    );
    return null;
  }
  try {
    await act(async () => root.render(createElement(Probe)));
    expect(request).toHaveBeenCalledTimes(2);
  } finally {
    await act(async () => root.unmount());
  }
});

it("does no hidden polling and refreshes on return without retaining listeners after close", async () => {
  vi.useFakeTimers();
  const hidden = vi.spyOn(document, "hidden", "get");
  const request = vi.fn().mockResolvedValue(1);
  const root = createRoot(document.createElement("div"));
  function Probe() {
    useSerialPoll(
      true,
      1000,
      request,
      () => {},
      () => {},
      "state",
    );
    return null;
  }
  await act(async () => root.render(createElement(Probe)));
  hidden.mockReturnValue(true);
  await act(async () => document.dispatchEvent(new Event("visibilitychange")));
  await act(async () => vi.advanceTimersByTime(300_000));
  await act(async () =>
    notifySyncInvalidation({ owner: "alice", kind: "state" }),
  );
  expect(request).toHaveBeenCalledTimes(1);
  hidden.mockReturnValue(false);
  await act(async () => document.dispatchEvent(new Event("visibilitychange")));
  expect(request).toHaveBeenCalledTimes(2);
  await act(async () => root.unmount());
  await act(async () => window.dispatchEvent(new Event("focus")));
  await act(async () =>
    notifySyncInvalidation({ owner: "alice", kind: "state" }),
  );
  expect(request).toHaveBeenCalledTimes(2);
});

it("drains a post-timeout invalidation after its global admission slot is released", async () => {
  vi.useFakeTimers();
  const releases: (() => void)[] = [];
  const held = Array.from({ length: 63 }, () =>
    readWithDeadline(
      () =>
        new Promise<void>((resolve) => {
          releases.push(resolve);
        }),
    ).catch(() => {}),
  );
  let finish!: (value: number) => void;
  const request = vi
    .fn()
    .mockImplementationOnce(
      () =>
        new Promise<number>((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue(2);
  const received = vi.fn();
  const root = createRoot(document.createElement("div"));
  function Probe() {
    useSerialPoll(true, 1000, request, received, () => {}, "state");
    return null;
  }
  try {
    await act(async () => root.render(createElement(Probe)));
    await act(async () => vi.advanceTimersByTime(10_000));
    await act(async () =>
      notifySyncInvalidation({ owner: "alice", kind: "state" }),
    );
    expect(request).toHaveBeenCalledTimes(1);
    await act(async () => finish(1));
    expect(request).toHaveBeenCalledTimes(2);
    expect(received).toHaveBeenCalledExactlyOnceWith(2);
  } finally {
    finish(1);
    releases.forEach((release) => release());
    await Promise.all(held);
    await act(async () => root.unmount());
  }
});
