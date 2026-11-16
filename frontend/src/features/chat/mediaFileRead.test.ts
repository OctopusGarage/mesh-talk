// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { notifySyncInvalidation } from "@/lib/syncInvalidation";
const fixture = vi.hoisted(() => ({
  readMediaPreview: vi.fn(),
  readMedia: vi.fn(),
  readFile: vi.fn(),
  current: true,
}));
vi.mock("@/lib/api", () => ({
  chat: {
    readMediaPreview: fixture.readMediaPreview,
    readMedia: fixture.readMedia,
    readFile: fixture.readFile,
  },
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
vi.mock("react-i18next", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-i18next")>()),
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

it("uses bounded preview bytes without original fallback or repeated decode on failure", async () => {
  fixture.readMediaPreview.mockRejectedValue(new Error("preview limit"));
  function Probe() {
    useFileObjectUrl("file", true, "image/png", { preview: true });
    return null;
  }
  await act(async () => root.render(createElement(Probe)));
  expect(fixture.readMediaPreview).toHaveBeenCalledOnce();
  expect(fixture.readMedia).not.toHaveBeenCalled();
  expect(fixture.readFile).not.toHaveBeenCalled();
  await act(async () => vi.advanceTimersByTimeAsync(5000));
  expect(fixture.readMediaPreview).toHaveBeenCalledOnce();
});

it("limits short commit-race retries and keeps permanent failures quiet for 30 seconds", async () => {
  function Probe() {
    useFileObjectUrl("file", true, "image/png");
    return null;
  }
  await act(async () => root.render(createElement(Probe)));
  await act(async () => vi.advanceTimersByTimeAsync(10_000));
  const initialAttempts = fixture.readMedia.mock.calls.length;
  expect(initialAttempts).toBeGreaterThan(1);
  expect(initialAttempts).toBeLessThanOrEqual(5);
  await act(async () => vi.advanceTimersByTimeAsync(30_000));
  expect(fixture.readMedia).toHaveBeenCalledTimes(initialAttempts);
});

it("recovers failed bytes promptly on an owned file invalidation, ignoring foreign and status noise", async () => {
  function Probe() {
    const url = useFileObjectUrl("file", true, "image/png");
    return createElement("span", null, url);
  }
  await act(async () => root.render(createElement(Probe)));
  expect(fixture.readMedia).toHaveBeenCalledOnce();
  fixture.readMedia.mockResolvedValue(new ArrayBuffer(1));
  await act(async () => {
    notifySyncInvalidation({ owner: "other", kind: "files" });
    notifySyncInvalidation({
      owner: "owner",
      kind: "state",
      scope: "sync_status",
    });
    notifySyncInvalidation({ owner: "owner", kind: "roster" });
  });
  expect(fixture.readMedia).toHaveBeenCalledOnce();
  await act(async () =>
    notifySyncInvalidation({ owner: "owner", kind: "files" }),
  );
  expect(fixture.readMedia).toHaveBeenCalledTimes(2);
  expect(host.textContent).toBe("blob:recovered");
  await act(async () => vi.advanceTimersByTimeAsync(310_000));
  expect(fixture.readMedia).toHaveBeenCalledTimes(2);
});

it("coalesces owned file events during a read into one serial follow-up after failure", async () => {
  let reject!: (error: Error) => void;
  let inFlight = 0;
  let maximumInFlight = 0;
  fixture.readMedia
    .mockImplementationOnce(() => {
      inFlight++;
      maximumInFlight = Math.max(maximumInFlight, inFlight);
      return new Promise<ArrayBuffer>((_, no) => {
        reject = no;
      }).finally(() => {
        inFlight--;
      });
    })
    .mockImplementation(() => {
      inFlight++;
      maximumInFlight = Math.max(maximumInFlight, inFlight);
      return Promise.resolve(new ArrayBuffer(1)).finally(() => {
        inFlight--;
      });
    });
  function Probe() {
    const url = useFileObjectUrl("file", true, "image/png");
    return createElement("span", null, url);
  }
  await act(async () => root.render(createElement(Probe)));
  await act(async () => {
    for (let i = 0; i < 3; i++)
      notifySyncInvalidation({ owner: "owner", kind: "files" });
  });
  expect(fixture.readMedia).toHaveBeenCalledOnce();
  expect(inFlight).toBe(1);
  await act(async () => reject(new Error("first read raced durable arrival")));
  await act(async () => vi.advanceTimersByTimeAsync(0));
  expect(fixture.readMedia).toHaveBeenCalledTimes(2);
  expect(maximumInFlight).toBe(1);
  expect(host.textContent).toBe("blob:recovered");
  expect(createUrl).toHaveBeenCalledOnce();
  await act(async () => vi.advanceTimersByTimeAsync(310_000));
  expect(fixture.readMedia).toHaveBeenCalledTimes(2);
});

it("uses foreground activation to recover failed bytes without waiting for a retry timer", async () => {
  const visible = vi
    .spyOn(document, "visibilityState", "get")
    .mockReturnValue("visible");
  function Probe() {
    const url = useFileObjectUrl("file", true, "image/png");
    return createElement("span", null, url);
  }
  try {
    await act(async () => root.render(createElement(Probe)));
    fixture.readMedia.mockResolvedValue(new ArrayBuffer(1));
    await act(async () => window.dispatchEvent(new Event("focus")));
    await act(async () => vi.advanceTimersByTimeAsync(0));
    expect(fixture.readMedia).toHaveBeenCalledTimes(2);
    expect(host.textContent).toBe("blob:recovered");
  } finally {
    visible.mockRestore();
  }
});

it("reserves lost-event recovery for a five-minute interval after the short burst", async () => {
  function Probe() {
    useFileObjectUrl("file", true, "image/png");
    return null;
  }
  await act(async () => root.render(createElement(Probe)));
  await act(async () => vi.advanceTimersByTimeAsync(10_000));
  const initialAttempts = fixture.readMedia.mock.calls.length;
  await act(async () => vi.advanceTimersByTimeAsync(289_000));
  expect(fixture.readMedia).toHaveBeenCalledTimes(initialAttempts);
  fixture.readMedia.mockResolvedValue(new ArrayBuffer(1));
  await act(async () => vi.advanceTimersByTimeAsync(20_000));
  expect(fixture.readMedia).toHaveBeenCalledTimes(initialAttempts + 1);
  await act(async () => vi.advanceTimersByTimeAsync(310_000));
  expect(fixture.readMedia).toHaveBeenCalledTimes(initialAttempts + 1);
});

it("retires queued file events and late read results with their mounted owner", async () => {
  let resolve!: (bytes: ArrayBuffer) => void;
  fixture.readMedia.mockImplementation(
    () =>
      new Promise<ArrayBuffer>((yes) => {
        resolve = yes;
      }),
  );
  const error = vi.fn();
  function Probe() {
    const url = useFileObjectUrl("file", true, "image/png", {
      onReadError: error,
    });
    return createElement("span", null, url);
  }
  await act(async () => root.render(createElement(Probe)));
  await act(async () =>
    notifySyncInvalidation({ owner: "owner", kind: "files" }),
  );
  fixture.current = false;
  await act(async () => root.render(null));
  await act(async () => resolve(new ArrayBuffer(1)));
  await act(async () => {
    notifySyncInvalidation({ owner: "owner", kind: "files" });
    window.dispatchEvent(new Event("focus"));
    await vi.advanceTimersByTimeAsync(310_000);
  });
  expect(fixture.readMedia).toHaveBeenCalledOnce();
  expect(fixture.readFile).not.toHaveBeenCalled();
  expect(error).not.toHaveBeenCalled();
  expect(createUrl).not.toHaveBeenCalled();
  expect(host.textContent).toBe("");
});
