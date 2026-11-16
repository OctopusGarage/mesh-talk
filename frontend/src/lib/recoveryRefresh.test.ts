import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { startRecoveryRefresh, RECOVERY_REFRESH_MS } from "./recoveryRefresh";
let stop: (() => void) | undefined;
let events: EventTarget;
let page: EventTarget & { visibilityState: string };
beforeEach(() => {
  vi.useFakeTimers();
  events = new EventTarget();
  page = Object.assign(new EventTarget(), { visibilityState: "visible" });
  vi.stubGlobal("window", events);
  vi.stubGlobal("document", page);
});
afterEach(() => {
  stop?.();
  stop = undefined;
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
it("does no eager work, coalesces activation, and keeps a minute-scale recovery", async () => {
  const run = vi.fn();
  stop = startRecoveryRefresh(run);
  await vi.advanceTimersByTimeAsync(30_000);
  expect(run).not.toHaveBeenCalled();
  events.dispatchEvent(new Event("focus"));
  page.dispatchEvent(new Event("visibilitychange"));
  await vi.advanceTimersByTimeAsync(0);
  expect(run).toHaveBeenCalledOnce();
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS);
  expect(run).toHaveBeenCalledTimes(2);
});
it("retires queued activation, fallback, and listeners with the owner", async () => {
  const run = vi.fn();
  stop = startRecoveryRefresh(run);
  events.dispatchEvent(new Event("focus"));
  stop();
  page.dispatchEvent(new Event("visibilitychange"));
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS * 2);
  expect(run).not.toHaveBeenCalled();
  expect(vi.getTimerCount()).toBe(0);
});
it("parks presentation-only recovery while hidden and refreshes on return", async () => {
  const run = vi.fn();
  stop = startRecoveryRefresh(run, { background: false });
  page.visibilityState = "hidden";
  page.dispatchEvent(new Event("visibilitychange"));
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS * 2);
  expect(run).not.toHaveBeenCalled();
  expect(vi.getTimerCount()).toBe(0);
  page.visibilityState = "visible";
  page.dispatchEvent(new Event("visibilitychange"));
  await vi.advanceTimersByTimeAsync(0);
  expect(run).toHaveBeenCalledOnce();
});
it("retains durable reconciliation recovery while hidden", async () => {
  const run = vi.fn();
  page.visibilityState = "hidden";
  stop = startRecoveryRefresh(run);
  events.dispatchEvent(new Event("focus"));
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS);
  expect(run).toHaveBeenCalledOnce();
});
