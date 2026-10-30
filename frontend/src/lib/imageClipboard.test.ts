// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { copyImage } from "./imageClipboard";
import { useSettings } from "@/store/settings";

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
it("starts a PNG clipboard write in the user gesture and rejects retired owners", async () => {
  useSettings.setState({
    privacy: {
      ...useSettings.getState().privacy,
      confirm_sensitive_copy: false,
      clipboard_clear_seconds: 0,
    },
  });
  const png = new Blob(["pixels"], { type: "image/png" });
  const canvas = {
    getContext: () => ({ drawImage: vi.fn() }),
    toBlob: (done: (blob: Blob) => void) => done(png),
  };
  vi.spyOn(document, "createElement").mockReturnValue(
    canvas as unknown as HTMLCanvasElement,
  );
  const image = { naturalWidth: 20, naturalHeight: 10 } as HTMLImageElement;
  const items: Record<string, Promise<Blob>>[] = [];
  vi.stubGlobal(
    "ClipboardItem",
    class {
      constructor(data: Record<string, Promise<Blob>>) {
        items.push(data);
      }
    },
  );
  const write = vi.fn(async () => {
    await items[0]["image/png"];
  });
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { write },
  });
  const copying = copyImage(image, "Confirm", () => true);
  expect(write).toHaveBeenCalledTimes(1);
  expect(await copying).toBe(true);
  expect(await items[0]["image/png"]).toBe(png);
  write.mockClear();
  write.mockRejectedValueOnce(new DOMException("Denied", "NotAllowedError"));
  await expect(copyImage(image, "Confirm", () => true)).rejects.toThrow(
    "Denied",
  );
  write.mockClear();
  expect(await copyImage(image, "Confirm", () => false)).toBe(false);
  expect(write).not.toHaveBeenCalled();
});
it("requires confirmation and surfaces unavailable clipboard support", async () => {
  useSettings.setState({
    privacy: {
      ...useSettings.getState().privacy,
      confirm_sensitive_copy: true,
    },
  });
  vi.spyOn(window, "confirm").mockReturnValue(false);
  expect(await copyImage({} as HTMLImageElement, "Confirm", () => true)).toBe(
    false,
  );
  vi.spyOn(window, "confirm").mockReturnValue(true);
  vi.stubGlobal("ClipboardItem", undefined);
  await expect(
    copyImage({} as HTMLImageElement, "Confirm", () => true),
  ).rejects.toThrow();
});

it("rejects stale ownership during PNG encoding", async () => {
  useSettings.setState({
    privacy: {
      ...useSettings.getState().privacy,
      confirm_sensitive_copy: false,
      clipboard_clear_seconds: 0,
    },
  });
  let finish!: (blob: Blob) => void;
  vi.spyOn(document, "createElement").mockReturnValue({
    getContext: () => ({ drawImage: vi.fn() }),
    toBlob: (done: typeof finish) => {
      finish = done;
    },
  } as unknown as HTMLCanvasElement);
  let data!: Record<string, Promise<Blob>>;
  vi.stubGlobal(
    "ClipboardItem",
    class {
      constructor(value: typeof data) {
        data = value;
      }
    },
  );
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { write: () => data["image/png"].then(() => {}) },
  });
  let current = true;
  const result = copyImage(
    { naturalWidth: 10, naturalHeight: 10 } as HTMLImageElement,
    "",
    () => current,
  );
  current = false;
  finish(new Blob(["png"]));
  await expect(result).rejects.toThrow("retired");
});

it("clears its unchanged PNG and preserves a newer clipboard image", async () => {
  vi.useFakeTimers();
  try {
    useSettings.setState({
      privacy: {
        ...useSettings.getState().privacy,
        confirm_sensitive_copy: false,
        clipboard_clear_seconds: 1,
      },
    });
    const png = {
      size: 2,
      arrayBuffer: async () => new Uint8Array([1, 2]).buffer,
    } as Blob;
    let candidate = png;
    vi.spyOn(document, "createElement").mockReturnValue({
      getContext: () => ({ drawImage: vi.fn() }),
      toBlob: (done: (blob: Blob) => void) => done(png),
    } as unknown as HTMLCanvasElement);
    vi.stubGlobal(
      "ClipboardItem",
      class {
        constructor(_value: unknown) {}
      },
    );
    const clear = vi.fn();
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        write: vi.fn(async () => {}),
        read: async () => [
          { types: ["image/png"], getType: async () => candidate },
        ],
        writeText: clear,
      },
    });
    const image = { naturalWidth: 10, naturalHeight: 10 } as HTMLImageElement;
    await copyImage(image, "", () => true);
    await vi.advanceTimersByTimeAsync(1000);
    expect(clear).toHaveBeenCalledWith("");
    clear.mockClear();
    await copyImage(image, "", () => true);
    candidate = {
      size: 2,
      arrayBuffer: async () => new Uint8Array([3, 4]).buffer,
    } as Blob;
    await vi.advanceTimersByTimeAsync(1000);
    expect(clear).not.toHaveBeenCalled();
  } finally {
    vi.useRealTimers();
  }
});
