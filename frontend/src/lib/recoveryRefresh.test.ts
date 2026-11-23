import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { startRecoveryRefresh, RECOVERY_REFRESH_MS } from "./recoveryRefresh";
const native = vi.hoisted(() => ({
  minimized: false,
  listeners: new Set<() => void>(),
}));
vi.mock("./nativeWindowVisibility", () => ({
  isNativeWindowMinimized: () => native.minimized,
  subscribeNativeWindowMinimized: (listener: () => void) => {
    native.listeners.add(listener);
    return () => native.listeners.delete(listener);
  },
}));

function setMinimized(value: boolean) {
  native.minimized = value;
  for (const listener of native.listeners) listener();
}
let stop: (() => void) | undefined;
let events: EventTarget;
let page: EventTarget & { visibilityState: string };
beforeEach(() => {
  vi.useFakeTimers();
  native.minimized = false;
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
  setMinimized(true);
  events.dispatchEvent(new Event("focus"));
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS);
  expect(run).toHaveBeenCalledOnce();
});

it("supports foreground-only recovery with no periodic timer", async () => {
  const run = vi.fn();
  stop = startRecoveryRefresh(run, { intervalMs: null });
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS * 2);
  expect(run).not.toHaveBeenCalled();
  expect(vi.getTimerCount()).toBe(0);
  page.visibilityState = "hidden";
  page.dispatchEvent(new Event("visibilitychange"));
  await vi.advanceTimersByTimeAsync(0);
  expect(run).not.toHaveBeenCalled();
  page.visibilityState = "visible";
  page.dispatchEvent(new Event("visibilitychange"));
  events.dispatchEvent(new Event("focus"));
  await vi.advanceTimersByTimeAsync(0);
  expect(run).toHaveBeenCalledOnce();
  stop();
  events.dispatchEvent(new Event("focus"));
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS);
  expect(run).toHaveBeenCalledOnce();
});

it("parks presentation recovery on native minimize even when the WebView reports visible", async () => {
  const run = vi.fn();
  stop = startRecoveryRefresh(run, { background: false });
  setMinimized(true);
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS * 2);
  expect(run).not.toHaveBeenCalled();
  expect(vi.getTimerCount()).toBe(0);
  setMinimized(false);
  await vi.advanceTimersByTimeAsync(0);
  expect(run).toHaveBeenCalledOnce();
  stop();
  setMinimized(true);
  setMinimized(false);
  await vi.advanceTimersByTimeAsync(RECOVERY_REFRESH_MS);
  expect(run).toHaveBeenCalledOnce();
});
