// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ImageQrResult } from "./ImageQrResult";
const fixture = vi.hoisted(() => ({
  current: true,
  openUrl: vi.fn(),
  open: vi.fn(),
  peers: [],
  myAccountId: "a".repeat(32),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: fixture.openUrl }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/store/chat", () => ({
  captureChatOwnership: () => ({ current: () => fixture.current }),
  useChat: Object.assign(
    (selector: (s: typeof fixture) => unknown) => selector(fixture),
    { getState: () => fixture },
  ),
}));
vi.mock("@/lib/rosterProjection", () => ({
  rosterProjection: () => ({ contactForAccount: () => false }),
}));
vi.mock("@/lib/displayIdentity", () => ({
  displayIdentity: () => ({ name: "My account" }),
}));
class ScanWorker {
  static latest: ScanWorker;
  onmessage?: (e: { data: string | null }) => void;
  onerror?: () => void;
  terminate = vi.fn();
  postMessage = vi.fn();
  constructor() {
    ScanWorker.latest = this;
  }
}
let host: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  fixture.current = true;
  fixture.open.mockReset();
  fixture.openUrl.mockReset().mockResolvedValue(undefined);
  vi.stubGlobal("Worker", ScanWorker);
  vi.stubGlobal(
    "Image",
    class {
      naturalWidth = 3000;
      naturalHeight = 1500;
      src = "";
      decode = vi.fn().mockResolvedValue(undefined);
    },
  );
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    fillStyle: "",
    fillRect: vi.fn(),
    drawImage: vi.fn(),
    getImageData: () => ({ data: new Uint8ClampedArray(4) }),
  } as unknown as CanvasRenderingContext2D);
  host = document.createElement("div");
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
async function render(url: string | null = "blob:fixture") {
  await act(async () =>
    root.render(
      createElement(ImageQrResult, {
        url,
        onClose: vi.fn(),
        onOpenChat: vi.fn(),
      }),
    ),
  );
}
async function result(data: string | null) {
  await act(async () => ScanWorker.latest.onmessage!({ data }));
}
it("bounds image pixels and offers verified own-account navigation", async () => {
  await render();
  expect(ScanWorker.latest.postMessage.mock.calls[0][0]).toMatchObject({
    width: 1536,
    height: 768,
  });
  await result(fixture.myAccountId.toUpperCase());
  await act(async () =>
    [...host.querySelectorAll("button")]
      .find((b) => b.textContent === "files.qrOpenChat")!
      .click(),
  );
  expect(fixture.open).toHaveBeenCalledWith({
    kind: "account",
    id: fixture.myAccountId,
    name: "My account",
  });
  expect(ScanWorker.latest.terminate).toHaveBeenCalledOnce();
});
it.each([
  "https://user:password@example.com",
  "https://",
  "hello world",
  "https://example.com/with space",
])("keeps untrusted QR contents as plain text: %s", async (value) => {
  await render();
  await result(value);
  expect(host.textContent).toContain(value);
  expect(host.textContent).not.toContain("files.qrOpenLink");
});
it("opens validated links and surfaces an opener failure", async () => {
  fixture.openUrl.mockRejectedValue(new Error("denied"));
  await render();
  await result("https://example.com");
  await act(async () =>
    [...host.querySelectorAll("button")]
      .find((b) => b.textContent === "files.qrOpenLink")!
      .click(),
  );
  expect(fixture.openUrl).toHaveBeenCalledWith("https://example.com/");
  expect(host.querySelector("[role=alert]")?.textContent).toBe(
    "files.qrOpenFailed",
  );
});
it("reports no QR and worker failure without keeping a worker alive", async () => {
  await render();
  await result(null);
  expect(host.textContent).toContain("files.qrNotFound");
  expect(ScanWorker.latest.terminate).toHaveBeenCalledOnce();
  await act(async () => root.render(null));
  await render();
  await act(async () => ScanWorker.latest.onerror!());
  expect(host.textContent).toContain("files.qrUnreadable");
});
it("does not publish results after owner retirement", async () => {
  await render();
  fixture.current = false;
  await result("retired private text");
  expect(host.textContent).not.toContain("retired private text");
});
it("expires an unavailable image rather than leaving scanning pending", async () => {
  vi.useFakeTimers();
  await render(null);
  await act(async () => vi.advanceTimersByTime(15000));
  expect(host.textContent).toContain("files.qrUnreadable");
});
it("reports unavailable canvas pixels", async () => {
  vi.mocked(HTMLCanvasElement.prototype.getContext).mockReturnValue(null);
  await render();
  expect(host.textContent).toContain("files.qrUnreadable");
});

it("keeps viewer keyboard shortcuts outside the QR panel while allowing Tab", async () => {
  const key = vi.fn();
  document.body.append(host);
  document.addEventListener("keydown", key);
  await render();
  const panel = host.querySelector("section")!;
  await act(async () =>
    panel.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  expect(key).not.toHaveBeenCalled();
  await act(async () =>
    panel.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Tab", bubbles: true }),
    ),
  );
  expect(key).toHaveBeenCalledOnce();
  document.removeEventListener("keydown", key);
  host.remove();
});

it("does not start an image scan for a retired owner", async () => {
  fixture.current = false;
  await render();
  expect(host.textContent).toContain("files.qrScanning");
});
it("cancels decoded image work when ownership changes before pixels are read", async () => {
  let decoded!: () => void;
  vi.stubGlobal(
    "Image",
    class {
      src = "";
      decode = () =>
        new Promise<void>((resolve) => {
          decoded = resolve;
        });
    },
  );
  await render();
  fixture.current = false;
  await act(async () => decoded());
  expect(HTMLCanvasElement.prototype.getContext).not.toHaveBeenCalled();
});
it("revalidates account and owner when a displayed chat action is clicked", async () => {
  await render();
  await result(fixture.myAccountId);
  const button = [...host.querySelectorAll("button")].find(
    (b) => b.textContent === "files.qrOpenChat",
  )!;
  fixture.current = false;
  await act(async () => button.click());
  expect(fixture.open).not.toHaveBeenCalled();
  fixture.current = true;
  const own = fixture.myAccountId;
  fixture.myAccountId = "f".repeat(32);
  try {
    await act(async () => button.click());
    expect(fixture.open).not.toHaveBeenCalled();
  } finally {
    fixture.myAccountId = own;
  }
});
it("does not open links or publish late opener errors for a retired owner", async () => {
  let reject!: (error: Error) => void;
  fixture.openUrl.mockReturnValue(
    new Promise<void>((_, fail) => {
      reject = fail;
    }),
  );
  await render();
  await result("https://example.com");
  const button = [...host.querySelectorAll("button")].find(
    (b) => b.textContent === "files.qrOpenLink",
  )!;
  fixture.current = false;
  await act(async () => button.click());
  expect(fixture.openUrl).not.toHaveBeenCalled();
  fixture.current = true;
  await act(async () => button.click());
  fixture.current = false;
  await act(async () => reject(new Error("late")));
  expect(host.querySelector("[role=alert]")).toBeNull();
});
