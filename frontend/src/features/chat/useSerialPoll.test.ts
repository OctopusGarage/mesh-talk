// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { useSerialPoll } from "./useSerialPoll";

afterEach(() => {
  vi.useRealTimers();
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
