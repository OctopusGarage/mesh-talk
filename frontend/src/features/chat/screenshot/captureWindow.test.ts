import { expect, it, vi } from "vitest";
import { expandCaptureWindow } from "./captureWindow";

it("leaves an existing fullscreen window unchanged", async () => {
  const window = {
    isFullscreen: vi.fn(async () => true),
    setSimpleFullscreen: vi.fn(async () => {}),
  };
  const restore = expandCaptureWindow(window);
  await vi.waitFor(() => expect(window.isFullscreen).toHaveBeenCalledOnce());
  restore();
  expect(window.setSimpleFullscreen).not.toHaveBeenCalled();
});
it("restores a temporary overlay when the editor closes", async () => {
  const window = {
    isFullscreen: vi.fn(async () => false),
    setSimpleFullscreen: vi.fn(async () => {}),
  };
  const restore = expandCaptureWindow(window);
  await vi.waitFor(() =>
    expect(window.setSimpleFullscreen).toHaveBeenCalledWith(true),
  );
  restore();
  await vi.waitFor(() =>
    expect(window.setSimpleFullscreen).toHaveBeenLastCalledWith(false),
  );
});
it("restores even when expansion completes after cancellation", async () => {
  let finish!: () => void;
  const pending = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const window = {
    isFullscreen: vi.fn(async () => false),
    setSimpleFullscreen: vi.fn((expanded: boolean) =>
      expanded ? pending : Promise.resolve(),
    ),
  };
  const restore = expandCaptureWindow(window);
  await vi.waitFor(() =>
    expect(window.setSimpleFullscreen).toHaveBeenCalledWith(true),
  );
  restore();
  finish();
  await vi.waitFor(() =>
    expect(window.setSimpleFullscreen).toHaveBeenLastCalledWith(false),
  );
});

it("shares temporary fullscreen ownership across overlapping editors of the same window", async () => {
  const native = {
    label: "overlap-test",
    isFullscreen: vi.fn(async () => false),
    setSimpleFullscreen: vi.fn(async () => {}),
  };
  const first = expandCaptureWindow(native);
  await vi.waitFor(() =>
    expect(native.setSimpleFullscreen).toHaveBeenCalledWith(true),
  );
  first();
  const second = expandCaptureWindow({ ...native });
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(native.setSimpleFullscreen.mock.calls).toEqual([[true]]);
  second();
  await vi.waitFor(() =>
    expect(native.setSimpleFullscreen.mock.calls).toEqual([[true], [false]]),
  );
  expect(native.isFullscreen).toHaveBeenCalledOnce();
});

it("re-expands a new capture after an older restoration already in flight", async () => {
  let finishRestore!: () => void;
  const restorePending = new Promise<void>((resolve) => {
    finishRestore = resolve;
  });
  const native = {
    label: "restore-test",
    isFullscreen: vi.fn(async () => false),
    setSimpleFullscreen: vi.fn(async (_expanded: boolean) => {}),
  };
  native.setSimpleFullscreen
    .mockImplementationOnce(async () => {})
    .mockImplementationOnce(() => restorePending);
  const first = expandCaptureWindow(native);
  await vi.waitFor(() =>
    expect(native.setSimpleFullscreen).toHaveBeenCalledWith(true),
  );
  first();
  await vi.waitFor(() =>
    expect(native.setSimpleFullscreen).toHaveBeenCalledWith(false),
  );
  const second = expandCaptureWindow({ ...native });
  finishRestore();
  await vi.waitFor(() =>
    expect(native.setSimpleFullscreen.mock.calls).toEqual([
      [true],
      [false],
      [true],
    ]),
  );
  second();
  await vi.waitFor(() =>
    expect(native.setSimpleFullscreen.mock.calls).toEqual([
      [true],
      [false],
      [true],
      [false],
    ]),
  );
});
