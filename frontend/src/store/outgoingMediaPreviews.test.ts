// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  getOutgoingMediaPreview,
  releaseOutgoingMediaPreview,
  resetOutgoingMediaPreviews,
  retainOutgoingMediaPreview,
  useOutgoingMediaPreview,
} from "./outgoingMediaPreviews";

const createUrl = vi.fn();
const revokeUrl = vi.fn();
const ownerCurrent = () => true;
const image = (size = 1, type = "image/png") =>
  new Blob([new Uint8Array(size)], { type });
const mib = 1024 * 1024;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
  let next = 0;
  createUrl.mockReset().mockImplementation(() => `blob:outgoing-${++next}`);
  revokeUrl.mockReset();
  vi.stubGlobal(
    "URL",
    class extends URL {
      static createObjectURL = createUrl;
      static revokeObjectURL = revokeUrl;
    },
  );
});

afterEach(() => {
  resetOutgoingMediaPreviews();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it("exposes one stable preview for the exact captured Blob", () => {
  const captured = image();
  retainOutgoingMediaPreview("one", captured, ownerCurrent);
  const entry = getOutgoingMediaPreview("one");

  expect(createUrl).toHaveBeenCalledExactlyOnceWith(captured);
  expect(createUrl.mock.calls[0]?.[0]).toBe(captured);
  expect(entry).toMatchObject({ url: "blob:outgoing-1" });
  expect(entry?.current()).toBe(true);
  expect(getOutgoingMediaPreview("one")).toBe(entry);
  expect(getOutgoingMediaPreview()).toBeUndefined();
});

it("rejects empty, oversized and non-image inputs before creating URLs", () => {
  for (const candidate of [
    image(0),
    image(3 * mib + 1),
    image(1, "video/mp4"),
  ]) {
    expect(() =>
      retainOutgoingMediaPreview("one", candidate, ownerCurrent),
    ).not.toThrow();
  }

  expect(createUrl).not.toHaveBeenCalled();
  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(vi.getTimerCount()).toBe(0);
});

it("leaves no entry or timer when object URLs are unavailable", () => {
  vi.stubGlobal("URL", class {});

  expect(() =>
    retainOutgoingMediaPreview("one", image(), ownerCurrent),
  ).not.toThrow();
  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(vi.getTimerCount()).toBe(0);
});

it("can retain a later image when object URL creation fails", () => {
  createUrl.mockImplementationOnce(() => {
    throw new Error("Failed to create URL");
  });

  expect(() =>
    retainOutgoingMediaPreview("one", image(), ownerCurrent),
  ).not.toThrow();
  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(vi.getTimerCount()).toBe(0);

  retainOutgoingMediaPreview("one", image(), ownerCurrent);
  expect(getOutgoingMediaPreview("one")?.url).toBe("blob:outgoing-1");
});

it("evicts the oldest displayed preview at the combined byte limit", () => {
  const maximum = image(3 * mib);
  const keys = ["first", "second", "third", "fourth"];
  for (const key of keys)
    retainOutgoingMediaPreview(key, maximum, ownerCurrent);
  const first = getOutgoingMediaPreview("first");

  expect(first?.current()).toBe(true);
  expect(revokeUrl).not.toHaveBeenCalled();
  let revocationsAtCreation = 0;
  createUrl.mockImplementationOnce(() => {
    revocationsAtCreation = revokeUrl.mock.calls.length;
    return "blob:fifth";
  });
  retainOutgoingMediaPreview("fifth", image(), ownerCurrent);

  expect(revocationsAtCreation).toBe(1);
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:outgoing-1");
  expect(first?.current()).toBe(false);
  expect(getOutgoingMediaPreview("first")).toBeUndefined();
  expect(getOutgoingMediaPreview("second")?.url).toBe("blob:outgoing-2");
  expect(getOutgoingMediaPreview("fifth")?.url).toBe("blob:fifth");
});

it("evicts the oldest preview when a ninth entry is retained", () => {
  const captured = image();
  for (let i = 0; i < 9; i++) {
    retainOutgoingMediaPreview(String(i), captured, ownerCurrent);
  }

  expect(getOutgoingMediaPreview("0")).toBeUndefined();
  expect(getOutgoingMediaPreview("1")?.url).toBe("blob:outgoing-2");
  expect(getOutgoingMediaPreview("8")?.url).toBe("blob:outgoing-9");
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:outgoing-1");
});

it("creates no URL for an already retired owner", () => {
  retainOutgoingMediaPreview("one", image(), () => false);

  expect(createUrl).not.toHaveBeenCalled();
  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(vi.getTimerCount()).toBe(0);
});

it("revokes an unpublished URL if its owner retires during creation", () => {
  let current = true;
  createUrl.mockImplementationOnce(() => {
    current = false;
    return "blob:retired";
  });
  retainOutgoingMediaPreview("one", image(), () => current);

  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:retired");
  expect(vi.getTimerCount()).toBe(0);
});

it("keeps a reset during URL creation from publishing the preview", () => {
  createUrl.mockImplementationOnce(() => {
    resetOutgoingMediaPreviews();
    return "blob:retired";
  });
  retainOutgoingMediaPreview("one", image(), ownerCurrent);

  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:retired");
  expect(vi.getTimerCount()).toBe(0);
});

it("refuses retired entries on read and reclaims them on mutation", () => {
  let current = true;
  retainOutgoingMediaPreview("one", image(), () => current);
  const entry = getOutgoingMediaPreview("one");
  current = false;

  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(entry?.current()).toBe(false);
  expect(revokeUrl).not.toHaveBeenCalled();

  retainOutgoingMediaPreview("two", image(), ownerCurrent);
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:outgoing-1");
  expect(getOutgoingMediaPreview("two")?.url).toBe("blob:outgoing-2");
});

it("uses one timer and expires each entry at its own deadline", () => {
  expect(vi.getTimerCount()).toBe(0);
  retainOutgoingMediaPreview("one", image(), ownerCurrent);
  const first = getOutgoingMediaPreview("one");
  vi.advanceTimersByTime(10_000);
  retainOutgoingMediaPreview("two", image(), ownerCurrent);
  expect(vi.getTimerCount()).toBe(1);

  vi.advanceTimersByTime(19_999);
  expect(first?.current()).toBe(true);
  expect(revokeUrl).not.toHaveBeenCalled();

  vi.advanceTimersByTime(1);
  expect(first?.current()).toBe(false);
  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(getOutgoingMediaPreview("two")?.url).toBe("blob:outgoing-2");
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:outgoing-1");
  expect(vi.getTimerCount()).toBe(1);

  vi.advanceTimersByTime(10_000);
  expect(getOutgoingMediaPreview("two")).toBeUndefined();
  expect(revokeUrl.mock.calls).toEqual([
    ["blob:outgoing-1"],
    ["blob:outgoing-2"],
  ]);
  expect(vi.getTimerCount()).toBe(0);
});

it("refuses expired entries even before the expiry timer runs", () => {
  retainOutgoingMediaPreview("one", image(), ownerCurrent);
  const entry = getOutgoingMediaPreview("one");
  vi.spyOn(performance, "now").mockReturnValue(performance.now() + 30_000);

  expect(entry?.current()).toBe(false);
  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(revokeUrl).not.toHaveBeenCalled();
});

it("synchronously revokes all entries and clears the outstanding timer at reset", () => {
  retainOutgoingMediaPreview("one", image(), ownerCurrent);
  retainOutgoingMediaPreview("two", image(), ownerCurrent);
  const entry = getOutgoingMediaPreview("one");

  resetOutgoingMediaPreviews();
  expect(entry?.current()).toBe(false);
  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(getOutgoingMediaPreview("two")).toBeUndefined();
  expect(revokeUrl.mock.calls).toEqual([
    ["blob:outgoing-1"],
    ["blob:outgoing-2"],
  ]);
  expect(vi.getTimerCount()).toBe(0);
  entry?.release();
  vi.advanceTimersByTime(30_000);
  expect(revokeUrl).toHaveBeenCalledTimes(2);
});

it("binds release to the exact entry when a key is replaced", () => {
  retainOutgoingMediaPreview("one", image(), ownerCurrent);
  const first = getOutgoingMediaPreview("one");
  retainOutgoingMediaPreview("one", image(), ownerCurrent);
  const second = getOutgoingMediaPreview("one");

  expect(first?.current()).toBe(false);
  first?.release();
  expect(getOutgoingMediaPreview("one")).toBe(second);
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:outgoing-1");

  second?.release();
  releaseOutgoingMediaPreview("one");
  expect(second?.current()).toBe(false);
  expect(getOutgoingMediaPreview("one")).toBeUndefined();
  expect(revokeUrl.mock.calls).toEqual([
    ["blob:outgoing-1"],
    ["blob:outgoing-2"],
  ]);
  expect(vi.getTimerCount()).toBe(0);
});

it("leaves an expired replacement for its own deadline to reclaim", () => {
  retainOutgoingMediaPreview("one", image(), ownerCurrent);
  const first = getOutgoingMediaPreview("one");
  retainOutgoingMediaPreview("one", image(), ownerCurrent);
  vi.spyOn(performance, "now").mockReturnValue(performance.now() + 30_000);

  first?.release();
  expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:outgoing-1");
  expect(vi.getTimerCount()).toBe(1);

  vi.advanceTimersByTime(30_000);
  expect(revokeUrl.mock.calls).toEqual([
    ["blob:outgoing-1"],
    ["blob:outgoing-2"],
  ]);
  expect(vi.getTimerCount()).toBe(0);
});

it("subscribes a mounted consumer to just its chosen entry", () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  retainOutgoingMediaPreview("one", image(), ownerCurrent);
  const root = createRoot(document.createElement("div"));
  const render = vi.fn();
  function Preview() {
    render(useOutgoingMediaPreview("one"));
    return null;
  }

  try {
    act(() => root.render(createElement(Preview)));
    expect(render).toHaveBeenCalledExactlyOnceWith(
      getOutgoingMediaPreview("one"),
    );

    act(() => retainOutgoingMediaPreview("two", image(), ownerCurrent));
    expect(render).toHaveBeenCalledTimes(1);

    act(() => releaseOutgoingMediaPreview("one"));
    expect(render).toHaveBeenCalledTimes(2);
    expect(render).toHaveBeenLastCalledWith(undefined);
  } finally {
    act(() => root.unmount());
  }
});
