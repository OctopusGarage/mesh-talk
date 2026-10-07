import { describe, expect, it, vi } from "vitest";
import { jumpToLatest } from "./jumpToLatest";

describe("jumpToLatest", () => {
  it("uses the browser animation frame for the measurement retry", () => {
    const scrollTo = vi.fn();
    const list = { scrollTo };
    const frame = vi.fn((callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    });
    vi.stubGlobal("requestAnimationFrame", frame);
    try {
      jumpToLatest(() => list);
      expect(frame).toHaveBeenCalledOnce();
      expect(scrollTo).toHaveBeenCalledTimes(2);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("scrolls to the actual bottom now and after the virtual list measures", () => {
    const scrollTo = vi.fn();
    const list = { scrollTo };
    let nextFrame: (() => void) | undefined;
    jumpToLatest(
      () => list,
      (callback) => {
        nextFrame = callback;
      },
    );

    expect(scrollTo).toHaveBeenCalledOnce();
    expect(scrollTo).toHaveBeenCalledWith({
      top: Number.MAX_SAFE_INTEGER,
      behavior: "auto",
    });
    expect(nextFrame).toBeTypeOf("function");
    nextFrame?.();
    expect(scrollTo).toHaveBeenCalledTimes(2);
  });

  it("does not scroll a conversation that closed or changed before the next frame", () => {
    const scrollTo = vi.fn();
    const list = { scrollTo };
    let current: typeof list | null = null;
    let nextFrame: (() => void) | undefined;
    jumpToLatest(
      () => current,
      (callback) => {
        nextFrame = callback;
      },
    );
    expect(scrollTo).not.toHaveBeenCalled();

    current = list;
    jumpToLatest(
      () => current,
      (callback) => {
        nextFrame = callback;
      },
    );
    expect(scrollTo).toHaveBeenCalledOnce();
    current = null;
    nextFrame?.();
    expect(scrollTo).toHaveBeenCalledOnce();
  });
});
