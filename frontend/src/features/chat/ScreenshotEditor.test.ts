// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
import { ScreenshotEditor } from "./ScreenshotEditor";

let root: Root;
let container: HTMLDivElement;
let image: { onload: (() => void) | null };
const send = vi.fn();
const cancel = vi.fn();
const context = new Proxy({}, { get: () => vi.fn(), set: () => true });
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.stubGlobal(
    "Image",
    class {
      naturalWidth = 320;
      naturalHeight = 180;
      onload = null;
      onerror = null;
      constructor() {
        // Keep the explicitly controlled image loader used by this component test.
        // eslint-disable-next-line @typescript-eslint/no-this-alias
        image = this;
      }
      set src(_value: string) {
        /* Explicitly resolve loading below. */
      }
    },
  );
  vi.stubGlobal("URL", {
    createObjectURL: () => "blob:test",
    revokeObjectURL: vi.fn(),
  });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(
    context as CanvasRenderingContext2D,
  );
  vi.spyOn(
    HTMLCanvasElement.prototype,
    "getBoundingClientRect",
  ).mockReturnValue({
    x: 0,
    y: 0,
    left: 0,
    top: 0,
    right: 320,
    bottom: 180,
    width: 320,
    height: 180,
    toJSON: () => ({}),
  });
  vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation(
    (callback) =>
      callback({
        arrayBuffer: async () => new Uint8Array([1, 2]).buffer,
      } as Blob),
  );
  HTMLCanvasElement.prototype.setPointerCapture = vi.fn();
  HTMLCanvasElement.prototype.releasePointerCapture = vi.fn();
  send.mockReset().mockResolvedValue(undefined);
  cancel.mockReset();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () =>
    root.render(
      createElement(ScreenshotEditor, {
        bytes: new Uint8Array([1]),
        onSend: send,
        onCancel: cancel,
      }),
    ),
  );
  await act(async () => image.onload?.());
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
function canvas() {
  return container.querySelector("canvas")!;
}
async function pointer(type: string, x: number, y: number) {
  await act(async () =>
    canvas().dispatchEvent(
      new MouseEvent(type, {
        bubbles: true,
        clientX: x,
        clientY: y,
        button: 0,
      }),
    ),
  );
}
async function select() {
  await pointer("pointerdown", 20, 30);
  await pointer("pointermove", 120, 110);
  await pointer("pointerup", 120, 110);
}
async function key(value: string) {
  await act(async () =>
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: value, bubbles: true }),
    ),
  );
}
it("shows pixel dimensions and a floating toolbar after selecting, then confirms with Enter", async () => {
  await select();
  expect(
    container.querySelector('[data-testid="screenshot-dimensions"]')
      ?.textContent,
  ).toBe("100 × 80");
  expect(
    container.querySelector('[data-testid="screenshot-toolbar"]'),
  ).not.toBeNull();
  await key("Enter");
  expect(send).toHaveBeenCalledOnce();
});
it("resizes a selection by its bottom-right handle", async () => {
  await select();
  await pointer("pointerdown", 120, 110);
  await pointer("pointermove", 150, 140);
  await pointer("pointerup", 150, 140);
  expect(
    container.querySelector('[data-testid="screenshot-dimensions"]')
      ?.textContent,
  ).toBe("130 × 110");
});
it("does not confirm before selection, on IME Enter, or when typing annotation text", async () => {
  await key("Enter");
  expect(send).not.toHaveBeenCalled();
  await select();
  await act(async () =>
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", isComposing: true }),
    ),
  );
  expect(send).not.toHaveBeenCalled();
  const textButton = container.querySelector<HTMLButtonElement>(
    '[aria-label="Text"]',
  )!;
  await act(async () => textButton.click());
  const input = container.querySelector("input[type=text]")!;
  await act(async () =>
    input.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
    ),
  );
  expect(send).not.toHaveBeenCalled();
});
it("cancels with Escape and never exports", async () => {
  await select();
  await key("Escape");
  expect(cancel).toHaveBeenCalledOnce();
  expect(send).not.toHaveBeenCalled();
});
it("keeps the selection and retry action when export fails", async () => {
  send.mockRejectedValueOnce(new Error("disk full"));
  await select();
  await key("Enter");
  expect(container.querySelector("[role=alert]")?.textContent).toContain(
    "disk full",
  );
  await key("Enter");
  expect(send).toHaveBeenCalledTimes(2);
});
it("prevents concurrent confirmations", async () => {
  send.mockImplementation(() => new Promise(() => {}));
  await select();
  await act(async () => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" }));
  });
  expect(send).toHaveBeenCalledOnce();
});

it("lets Enter activate a focused Cancel button instead of confirming", async () => {
  await select();
  const button = container.querySelector<HTMLButtonElement>(
    '[aria-label="Cancel"]',
  )!;
  button.focus();
  const event = new KeyboardEvent("keydown", {
    key: "Enter",
    bubbles: true,
    cancelable: true,
  });
  await act(async () => button.dispatchEvent(event));
  expect(event.defaultPrevented).toBe(false);
  expect(send).not.toHaveBeenCalled();
  await act(async () => button.click());
  expect(cancel).toHaveBeenCalledOnce();
});
