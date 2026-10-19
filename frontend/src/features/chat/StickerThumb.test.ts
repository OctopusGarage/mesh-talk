// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const preference = vi.hoisted(() => ({ motionOK: false }));
vi.mock("@/lib/motion", () => ({ useMotionOK: () => preference.motionOK }));
import { StickerThumb } from "./StickerThumb";

let root: Root;
let host: HTMLDivElement;
let mounted: boolean;
const fetchImage = vi.fn();
const decode = vi.fn();
const drawImage = vi.fn();
const close = vi.fn();
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
const bitmap = () => ({ width: 16, height: 24, close });
async function render(src = "animated.webp") {
  await act(async () =>
    root.render(
      createElement(StickerThumb, { src, alt: "Wave", fallback: "👋" }),
    ),
  );
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("fetch", fetchImage);
  vi.stubGlobal("createImageBitmap", decode);
  preference.motionOK = false;
  fetchImage
    .mockReset()
    .mockResolvedValue({ blob: async () => new Blob(["image"]) });
  decode.mockReset().mockResolvedValue(bitmap());
  drawImage.mockReset();
  close.mockReset();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    drawImage,
  } as unknown as CanvasRenderingContext2D);
  vi.spyOn(HTMLCanvasElement.prototype, "toDataURL").mockReturnValue(
    "data:image/webp;base64,static",
  );
  host = document.createElement("div");
  root = createRoot(host);
  mounted = true;
});
afterEach(async () => {
  if (mounted) await act(async () => root.unmount());
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
it("renders the original animated source when motion is permitted", async () => {
  preference.motionOK = true;
  await render();
  const image = host.querySelector("img")!;
  expect(image.getAttribute("src")).toBe("animated.webp");
  expect(image.getAttribute("alt")).toBe("Wave");
  expect(image.getAttribute("loading")).toBe("lazy");
  expect(fetchImage).not.toHaveBeenCalled();
});
it("shows a conservative static fallback while the first frame is decoding", async () => {
  const pending = deferred<ReturnType<typeof bitmap>>();
  decode.mockReturnValue(pending.promise);
  await render();
  expect(host.querySelector("img")).toBeNull();
  expect(host.textContent).toBe("👋");
  await act(async () => pending.resolve(bitmap()));
  const image = host.querySelector("img")!;
  expect(image.getAttribute("src")).toBe("data:image/webp;base64,static");
  expect(image.getAttribute("loading")).toBe("eager");
  expect(close).toHaveBeenCalledOnce();
});
it("follows live preference changes through the shared motion hook", async () => {
  preference.motionOK = true;
  await render();
  preference.motionOK = false;
  await render();
  expect(host.querySelector("img")?.getAttribute("src")).toBe(
    "data:image/webp;base64,static",
  );
  preference.motionOK = true;
  await render();
  expect(host.querySelector("img")?.getAttribute("src")).toBe("animated.webp");
});
it.each(["fetch", "decode", "draw", "encode", "context"])(
  "keeps the fallback after %s failure",
  async (stage) => {
    if (stage === "fetch") fetchImage.mockRejectedValue(new Error("network"));
    if (stage === "decode") decode.mockRejectedValue(new Error("decode"));
    if (stage === "draw")
      drawImage.mockImplementation(() => {
        throw new Error("draw");
      });
    if (stage === "encode")
      vi.mocked(HTMLCanvasElement.prototype.toDataURL).mockImplementation(
        () => {
          throw new Error("encode");
        },
      );
    if (stage === "context")
      vi.mocked(HTMLCanvasElement.prototype.getContext).mockReturnValue(null);
    await render();
    expect(host.querySelector("img")).toBeNull();
    expect(host.textContent).toBe("👋");
    if (["draw", "encode", "context"].includes(stage))
      expect(close).toHaveBeenCalledOnce();
  },
);
it("closes a bitmap decoded after unmount without drawing it", async () => {
  const pending = deferred<ReturnType<typeof bitmap>>();
  decode.mockReturnValue(pending.promise);
  await render();
  await act(async () => root.unmount());
  mounted = false;
  await act(async () => pending.resolve(bitmap()));
  expect(close).toHaveBeenCalledOnce();
  expect(drawImage).not.toHaveBeenCalled();
  expect(host.childElementCount).toBe(0);
});
it("ignores a stale decode after motion becomes allowed", async () => {
  const pending = deferred<ReturnType<typeof bitmap>>();
  decode.mockReturnValue(pending.promise);
  await render();
  preference.motionOK = true;
  await render();
  await act(async () => pending.resolve(bitmap()));
  expect(host.querySelector("img")?.getAttribute("src")).toBe("animated.webp");
  expect(close).toHaveBeenCalledOnce();
  expect(drawImage).not.toHaveBeenCalled();
});
it("discards a previous sticker's late decode after changing source", async () => {
  const pending = deferred<ReturnType<typeof bitmap>>();
  decode
    .mockReturnValueOnce(pending.promise)
    .mockRejectedValueOnce(new Error("new decode failed"));
  await render("old.webp");
  await render("new.webp");
  await act(async () => pending.resolve(bitmap()));
  expect(host.querySelector("img")).toBeNull();
  expect(host.textContent).toBe("👋");
  expect(close).toHaveBeenCalledOnce();
});
it("uses the fallback without fetching when a sticker has no image source", async () => {
  await render("");
  expect(host.querySelector("img")).toBeNull();
  expect(host.textContent).toBe("👋");
  expect(fetchImage).not.toHaveBeenCalled();
});
