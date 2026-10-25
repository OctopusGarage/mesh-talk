// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const fixture = vi.hoisted(() => ({
  readMedia: vi.fn(),
  readFile: vi.fn(),
  current: true,
}));
vi.mock("@/lib/api", () => ({
  chat: { readMedia: fixture.readMedia, readFile: fixture.readFile },
}));
vi.mock("@/store/auth", () => ({
  useAuth: (selector: (state: unknown) => unknown) =>
    selector({ user: { id: "owner" }, generation: 0 }),
}));
vi.mock("@/store/chat", () => ({
  useChat: Object.assign(
    (selector: (state: unknown) => unknown) =>
      selector({ runEpoch: 0, identityEpoch: 0, ready: true }),
    { getState: () => ({ runEpoch: 0 }) },
  ),
  captureChatOwnership: () => ({ current: () => fixture.current }),
}));
vi.mock("@/store/fileAvailability", () => ({
  useFileStatus: () => ({ ready: true }),
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
import { useFileObjectUrl } from "./mediaFile";
import { MediaSticker } from "./MediaSticker";
let root: Root;
let host: HTMLDivElement;
const createUrl = vi.fn(() => "blob:recovered");
const revokeUrl = vi.fn();
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("URL", {
    createObjectURL: createUrl,
    revokeObjectURL: revokeUrl,
  });
  fixture.current = true;
  fixture.readMedia.mockReset().mockRejectedValue(new Error("invalid sticker"));
  fixture.readFile.mockReset().mockRejectedValue(new Error("invalid sticker"));
  createUrl.mockClear();
  revokeUrl.mockClear();
  host = document.createElement("div");
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
it("reports admitted read failure without restarting on callback identity, and retries on request", async () => {
  const error = vi.fn();
  function Probe({ retryKey }: { retryKey: number }) {
    const url = useFileObjectUrl("file", true, "image/png", {
      onReadError: (failure) => error(failure),
      retryKey,
    });
    return createElement("span", null, url);
  }
  await act(async () => root.render(createElement(Probe, { retryKey: 0 })));
  expect(error).toHaveBeenCalledOnce();
  expect(fixture.readFile).toHaveBeenCalledOnce();
  await act(async () => root.render(createElement(Probe, { retryKey: 0 })));
  expect(fixture.readMedia).toHaveBeenCalledOnce();
  fixture.readMedia.mockResolvedValue(new ArrayBuffer(1));
  await act(async () => root.render(createElement(Probe, { retryKey: 1 })));
  expect(fixture.readMedia).toHaveBeenCalledTimes(2);
  expect(host.textContent).toBe("blob:recovered");
  await act(async () => vi.advanceTimersByTimeAsync(1000));
  expect(fixture.readMedia).toHaveBeenCalledTimes(2);
  await act(async () => root.unmount());
  expect(revokeUrl).toHaveBeenCalledWith("blob:recovered");
  root = createRoot(host);
});
it("shows read failure instead of endless loading and Retry performs a new read", async () => {
  await act(async () =>
    root.render(
      createElement(MediaSticker, {
        fileConv: "file",
        mime: "image/png",
        fallback: "👋",
      }),
    ),
  );
  expect(host.textContent).toContain("files.unavailable");
  expect(host.textContent).not.toContain("common.loading");
  expect(host.querySelector("button")?.textContent).toBe("common.retry");
  fixture.readMedia.mockResolvedValue(new ArrayBuffer(1));
  await act(async () => host.querySelector("button")!.click());
  expect(fixture.readMedia).toHaveBeenCalledTimes(2);
  expect(host.querySelector("img")?.getAttribute("src")).toBe("blob:recovered");
});
it("recovers a failed read in the background and retains read-only behavior", async () => {
  await act(async () =>
    root.render(
      createElement(MediaSticker, {
        fileConv: "file",
        mime: "image/png",
        fallback: "👋",
        readOnly: true,
      }),
    ),
  );
  expect(host.textContent).toContain("files.unavailable");
  expect(host.querySelector("button")).toBeNull();
  fixture.readMedia.mockResolvedValue(new ArrayBuffer(1));
  await act(async () => vi.advanceTimersByTimeAsync(200));
  expect(host.querySelector("img")).not.toBeNull();
});
it("does not report stale read errors or schedule work after ownership changes", async () => {
  let reject!: (error: Error) => void;
  fixture.readMedia.mockImplementation(
    () =>
      new Promise((_, no) => {
        reject = no;
      }),
  );
  const error = vi.fn();
  function Probe() {
    useFileObjectUrl("file", true, "image/png", { onReadError: error });
    return null;
  }
  await act(async () => root.render(createElement(Probe)));
  fixture.current = false;
  await act(async () => reject(new Error("late")));
  expect(error).not.toHaveBeenCalled();
  expect(fixture.readFile).not.toHaveBeenCalled();
  await act(async () => vi.advanceTimersByTimeAsync(5000));
  expect(fixture.readMedia).toHaveBeenCalledOnce();
});
