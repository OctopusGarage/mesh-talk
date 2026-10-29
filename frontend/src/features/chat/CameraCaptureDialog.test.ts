// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
import { CameraCaptureDialog } from "./CameraCaptureDialog";
let root: Root;
let host: HTMLDivElement;
const stop = vi.fn();
const confirm = vi.fn();
const getUserMedia = vi.fn();
const stream = () =>
  ({
    getTracks: () => [{ stop }],
    getVideoTracks: () => [{ getSettings: () => ({ facingMode: "user" }) }],
  }) as unknown as MediaStream;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("URL", {
    createObjectURL: () => "blob:photo",
    revokeObjectURL: vi.fn(),
  });
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: { getUserMedia, enumerateDevices: vi.fn().mockResolvedValue([]) },
  });
  vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    drawImage: vi.fn(),
  } as unknown as CanvasRenderingContext2D);
  vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation((cb) =>
    cb(new Blob(["photo"], { type: "image/jpeg" })),
  );
  stop.mockClear();
  confirm.mockClear();
  getUserMedia.mockReset().mockResolvedValue(stream());
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
async function render() {
  await act(async () =>
    root.render(
      createElement(CameraCaptureDialog, {
        onCancel: vi.fn(),
        onConfirm: confirm,
        onPickImage: vi.fn(),
      }),
    ),
  );
}
async function click(id: string) {
  await act(async () =>
    document.querySelector<HTMLButtonElement>(`[data-testid="${id}"]`)!.click(),
  );
}
it("releases a late camera permission result after cancellation", async () => {
  let resolve!: (s: MediaStream) => void;
  getUserMedia.mockReturnValue(
    new Promise<MediaStream>((r) => {
      resolve = r;
    }),
  );
  await render();
  await act(async () => root.render(null));
  await act(async () => resolve(stream()));
  expect(stop).toHaveBeenCalledOnce();
  expect(confirm).not.toHaveBeenCalled();
});
it("captures locally, releases the camera and waits for explicit confirmation", async () => {
  await render();
  const video = document.querySelector("video")!;
  Object.defineProperties(video, {
    videoWidth: { value: 1280 },
    videoHeight: { value: 720 },
  });
  await act(async () => video.dispatchEvent(new Event("loadeddata")));
  await click("camera-shutter");
  expect(confirm).not.toHaveBeenCalled();
  expect(stop).toHaveBeenCalledOnce();
  expect(document.querySelector('[data-testid="camera-photo"]')).not.toBeNull();
  await click("camera-use-photo");
  expect(confirm.mock.calls[0][0]).toBeInstanceOf(File);
});
it("allows retaking without submitting the previous photo", async () => {
  await render();
  const video = document.querySelector("video")!;
  Object.defineProperties(video, {
    videoWidth: { value: 640 },
    videoHeight: { value: 480 },
  });
  await act(async () => video.dispatchEvent(new Event("loadeddata")));
  await click("camera-shutter");
  await click("camera-retake");
  expect(getUserMedia).toHaveBeenCalledTimes(2);
  expect(document.querySelector("video")?.srcObject).not.toBeNull();
  expect(confirm).not.toHaveBeenCalled();
});
it("offers recovery when permission is denied without requesting audio", async () => {
  getUserMedia.mockRejectedValue(new DOMException("denied", "NotAllowedError"));
  await render();
  expect(document.querySelector('[role="alert"]')?.textContent).toContain(
    "permission",
  );
  expect(
    document.querySelector('[data-testid="camera-pick-image"]'),
  ).not.toBeNull();
  expect(getUserMedia.mock.calls[0][0].audio).toBe(false);
});

it("discards a late encoded photo after the dialog closes", async () => {
  let encode!: BlobCallback;
  vi.mocked(HTMLCanvasElement.prototype.toBlob).mockImplementation(
    (callback) => {
      encode = callback;
    },
  );
  await render();
  const video = document.querySelector("video")!;
  Object.defineProperties(video, {
    videoWidth: { value: 640 },
    videoHeight: { value: 480 },
  });
  await act(async () => video.dispatchEvent(new Event("loadeddata")));
  await click("camera-shutter");
  await act(async () => root.render(null));
  await act(async () => encode(new Blob(["late"], { type: "image/jpeg" })));
  expect(confirm).not.toHaveBeenCalled();
  expect(stop).toHaveBeenCalledOnce();
  expect(document.querySelector('[data-testid="camera-photo"]')).toBeNull();
});
