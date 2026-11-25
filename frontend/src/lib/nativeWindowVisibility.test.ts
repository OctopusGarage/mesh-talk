// @vitest-environment jsdom
import { beforeEach, expect, it, vi } from "vitest";
const native = vi.hoisted(() => ({
  isTauri: vi.fn(),
  listen: vi.fn(),
  isMinimized: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: native.isTauri }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ isMinimized: native.isMinimized }),
}));
beforeEach(() => {
  vi.resetModules();
  vi.resetAllMocks();
  native.isTauri.mockReturnValue(true);
});
it("does not subscribe in a browser", async () => {
  native.isTauri.mockReturnValue(false);
  const v = await import("./nativeWindowVisibility");
  expect(v.startNativeWindowVisibility()).toBeUndefined();
  expect(native.listen).not.toHaveBeenCalled();
});
it("publishes changes once and unsubscribes on cleanup", async () => {
  const unlisten = vi.fn();
  native.listen.mockResolvedValue(unlisten);
  native.isMinimized.mockResolvedValue(true);
  const v = await import("./nativeWindowVisibility");
  const observer = vi.fn();
  const unsubscribe = v.subscribeNativeWindowMinimized(observer);
  const stop = v.startNativeWindowVisibility()!;
  await vi.waitFor(() => expect(observer).toHaveBeenCalledOnce());
  expect(v.isNativeWindowMinimized()).toBe(true);
  const event = native.listen.mock.calls[0][1];
  event({ payload: true });
  expect(observer).toHaveBeenCalledOnce();
  event({ payload: false });
  expect(observer).toHaveBeenCalledTimes(2);
  unsubscribe();
  event({ payload: true });
  expect(observer).toHaveBeenCalledTimes(2);
  stop();
  await Promise.resolve();
  expect(unlisten).toHaveBeenCalledOnce();
  event({ payload: false });
  expect(v.isNativeWindowMinimized()).toBe(true);
});
it("keeps an event newer than the initial query", async () => {
  let resolve!: (v: boolean) => void;
  native.listen.mockResolvedValue(vi.fn());
  native.isMinimized.mockReturnValue(
    new Promise<boolean>((done) => {
      resolve = done;
    }),
  );
  const v = await import("./nativeWindowVisibility");
  const stop = v.startNativeWindowVisibility()!;
  await Promise.resolve();
  native.listen.mock.calls[0][1]({ payload: true });
  resolve(false);
  await Promise.resolve();
  expect(v.isNativeWindowMinimized()).toBe(true);
  stop();
});
it("retires an unresolved subscription without querying", async () => {
  let resolve!: (v: () => void) => void;
  const unlisten = vi.fn();
  native.listen.mockReturnValue(
    new Promise<() => void>((done) => {
      resolve = done;
    }),
  );
  const v = await import("./nativeWindowVisibility");
  v.startNativeWindowVisibility()!();
  resolve(unlisten);
  await Promise.resolve();
  await Promise.resolve();
  expect(unlisten).toHaveBeenCalledOnce();
  expect(native.isMinimized).not.toHaveBeenCalled();
});
it("tolerates failed subscription during retirement", async () => {
  native.listen.mockRejectedValue(new Error("unavailable"));
  const v = await import("./nativeWindowVisibility");
  v.startNativeWindowVisibility()!();
  await Promise.resolve();
  await Promise.resolve();
  expect(native.isMinimized).not.toHaveBeenCalled();
});
