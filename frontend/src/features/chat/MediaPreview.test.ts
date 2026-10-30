// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { FileStatus } from "@/lib/types";

const state = vi.hoisted(() => ({
  mac: true,
  current: true,
  confirm: false,
  url: "blob:ordinary-image" as string | null,
  status: {
    file_conv: "ordinary-image",
    done: 4,
    total: 4,
    ready: true,
  } as FileStatus,
  setError: vi.fn(),
  writeText: vi.fn(async (_text: string) => {}),
  writeSelection: vi.fn((_type: string, _value: string) => {}),
}));
vi.mock("react-i18next", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-i18next")>()),
  useTranslation: () => ({ t: (key: string) => key }),
}));
// These tests exercise selection/copy ownership; browser scenarios cover transitions.
vi.mock("framer-motion", () => {
  const element = (tag: string) => (props: Record<string, unknown>) =>
    createElement(
      tag,
      Object.fromEntries(
        Object.entries(props).filter(
          ([key]) =>
            !["initial", "animate", "exit", "layoutId", "transition"].includes(
              key,
            ),
        ),
      ),
    );
  return {
    AnimatePresence: ({ children }: { children: ReactNode }) => children,
    motion: { div: element("div"), img: element("img") },
  };
});
vi.mock("@/lib/platform", () => ({ isMacOverlay: () => state.mac }));
vi.mock("@/lib/motion", () => ({
  ease: [0, 0, 1, 1],
  useMotionOK: () => false,
}));
vi.mock("@/lib/attachmentSave", () => ({ saveAttachment: vi.fn() }));
vi.mock("@/lib/savedDownloads", () => ({ useSavedDownloads: () => ({}) }));
vi.mock("@/store/chat", () => ({
  useChat: (select: (value: { setError: typeof state.setError }) => unknown) =>
    select({ setError: state.setError }),
  captureChatOwnership: () => ({ current: () => state.current }),
}));
vi.mock("@/store/settings", () => ({
  useSettings: {
    getState: () => ({
      privacy: {
        confirm_sensitive_copy: state.confirm,
        clipboard_clear_seconds: 0,
      },
    }),
  },
}));
vi.mock("@/store/fileAvailability", () => ({
  useFileStatus: () => state.status,
  markUnavailableFileError: () => false,
}));
vi.mock("./mediaFile", () => ({
  blobMime: () => "image/png",
  useFileObjectUrl: (_fileConv: string, enabled: boolean) =>
    enabled ? state.url : null,
  useVideoPoster: () => ({ poster: null, failed: false }),
  withinInlineCap: () => true,
}));
import { MediaPreview } from "./MediaPreview";

let host: HTMLDivElement;
let root: Root;
const ancestorMenu = vi.fn();
let imageSequence = 0;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  ancestorMenu.mockClear();
  document.body.addEventListener("contextmenu", ancestorMenu);
  state.mac = true;
  state.current = true;
  state.confirm = false;
  state.url = `blob:ordinary-image-${++imageSequence}`;
  state.status = {
    file_conv: "ordinary-image",
    done: 4,
    total: 4,
    ready: true,
  };
  state.setError.mockReset();
  state.writeText.mockReset().mockResolvedValue(undefined);
  state.writeSelection.mockReset();
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: state.writeText },
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  document.body.removeEventListener("contextmenu", ancestorMenu);
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
async function renderImage(readOnly = false) {
  await act(async () =>
    root.render(
      createElement(MediaPreview, {
        fileConv: "ordinary-image",
        name: "photo.png",
        size: 100,
        mime: "image/png",
        readOnly,
      }),
    ),
  );
}
async function loadImage(image: HTMLImageElement, width = 800, height = 600) {
  Object.defineProperties(image, {
    naturalWidth: { configurable: true, value: width },
    naturalHeight: { configurable: true, value: height },
  });
  await act(async () => image.dispatchEvent(new Event("load")));
}
async function openImage() {
  await renderImage();
  await loadImage(
    host.querySelector<HTMLImageElement>('[data-testid="file-image"]')!,
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        'button[aria-label="files.viewFullSize"]',
      )!
      .click(),
  );
  return host.querySelector<HTMLImageElement>(
    '[data-testid="file-image-lightbox"] img',
  )!;
}

it("keeps the preparing state until the received image can actually display", async () => {
  state.url = null;
  await renderImage();
  expect(host.textContent).toContain("transfer.imagePreparing");
  state.url = `blob:pending-image-${imageSequence}`;
  await renderImage();
  const image = host.querySelector<HTMLImageElement>(
    '[data-testid="file-image"]',
  )!;
  const trigger = image.closest("button")!;
  expect(host.textContent).toContain("transfer.imagePreparing");
  expect(trigger.disabled).toBe(true);
  expect(image.style.opacity).toBe("0");
  await loadImage(image);
  expect(trigger.disabled).toBe(false);
  expect(image.style.opacity).toBe("1");
  expect(host.textContent).not.toContain("transfer.imagePreparing");
});

it("reuses decoded geometry without replaying preparation on remount", async () => {
  await renderImage();
  const image = host.querySelector<HTMLImageElement>(
    '[data-testid="file-image"]',
  )!;
  await loadImage(image, 1600, 200);
  expect(
    image.closest<HTMLElement>("[data-inline-image]")!.style.aspectRatio,
  ).toBe("1600 / 200");
  await act(async () => root.render(null));
  await renderImage();
  const reused = host.querySelector<HTMLImageElement>(
    '[data-testid="file-image"]',
  )!;
  expect(reused.style.opacity).toBe("1");
  expect(
    reused.closest<HTMLElement>("[data-inline-image]")!.style.aspectRatio,
  ).toBe("1600 / 200");
  expect(host.textContent).not.toContain("transfer.imagePreparing");
});

it("prepares a replacement URL and does not carry an old decode failure forward", async () => {
  await renderImage();
  const image = host.querySelector<HTMLImageElement>(
    '[data-testid="file-image"]',
  )!;
  await act(async () => image.dispatchEvent(new Event("error")));
  expect(host.textContent).toContain("files.imageUnsupported");
  const failure = host.querySelector('[role="status"]');
  expect(failure?.textContent).toBe("files.imageUnsupported");
  expect(failure?.querySelector("button")).toBeNull();
  state.url = `blob:replacement-${imageSequence}`;
  await renderImage();
  expect(host.textContent).not.toContain("files.imageUnsupported");
  expect(host.textContent).toContain("transfer.imagePreparing");
  const replacement = host.querySelector<HTMLImageElement>(
    '[data-testid="file-image"]',
  )!;
  await loadImage(replacement, 200, 800);
  expect(
    replacement.closest<HTMLElement>("[data-inline-image]")!.style.aspectRatio,
  ).toBe("200 / 800");
});

it("ignores image load and failure events after the captured session retires", async () => {
  await renderImage();
  const image = host.querySelector<HTMLImageElement>(
    '[data-testid="file-image"]',
  )!;
  state.current = false;
  await loadImage(image);
  await act(async () => image.dispatchEvent(new Event("error")));
  expect(image.style.opacity).toBe("0");
  expect(host.textContent).not.toContain("files.imageUnsupported");
});

it("keeps history images noninteractive through preparation and display", async () => {
  await renderImage(true);
  expect(host.querySelector("button")).toBeNull();
  expect(host.textContent).toContain("transfer.imagePreparing");
  const image = host.querySelector<HTMLImageElement>(
    '[data-testid="file-image"]',
  )!;
  await loadImage(image);
  expect(host.textContent).not.toContain("transfer.imagePreparing");
  expect(image.style.opacity).toBe("1");
  expect(host.querySelector("button")).toBeNull();
});
async function copySelection(image: HTMLImageElement, text = "图片里的文字") {
  // WebKit's UA image overlay exposes its selected text through this public API.
  // The native probe separately exercises real recognition and drag selection.
  vi.spyOn(window, "getSelection").mockReturnValue({
    toString: () => text,
    removeAllRanges: vi.fn(),
  } as unknown as Selection);
  const event = new Event("copy", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    value: { setData: state.writeSelection },
  });
  await act(async () => {
    image.dispatchEvent(event);
  });
  return event;
}

it("keeps native text selection while replacing the browser image menu", async () => {
  const image = await openImage();
  expect(image.classList.contains("select-text")).toBe(true);
  expect(
    host
      .querySelector('[data-testid="file-image"]')!
      .classList.contains("select-text"),
  ).toBe(false);
  const context = new MouseEvent("contextmenu", {
    bubbles: true,
    cancelable: true,
  });
  image.dispatchEvent(context);
  expect(context.defaultPrevented).toBe(true);
  expect(ancestorMenu).not.toHaveBeenCalled();
});

it("routes native image text copy through the existing clipboard policy", async () => {
  const image = await openImage();
  const event = await copySelection(image);
  expect(event.defaultPrevented).toBe(true);
  expect(state.writeSelection).toHaveBeenCalledWith(
    "text/plain",
    "图片里的文字",
  );
  expect(state.writeText).not.toHaveBeenCalled();
  state.confirm = true;
  vi.spyOn(window, "confirm").mockReturnValue(false);
  state.writeSelection.mockClear();
  await copySelection(image);
  expect(window.confirm).toHaveBeenCalledWith("settings.confirmCopyPrompt");
  expect(state.writeSelection).not.toHaveBeenCalled();
});

it("blocks stale session copies and reports current clipboard failures", async () => {
  const image = await openImage();
  state.current = false;
  expect((await copySelection(image)).defaultPrevented).toBe(true);
  expect(state.writeSelection).not.toHaveBeenCalled();
  state.current = true;
  state.writeSelection.mockImplementationOnce(() => {
    throw new Error("clipboard unavailable");
  });
  await copySelection(image);
  expect(state.setError).toHaveBeenCalledWith("common.copyFailed");
});

it("isolates image actions from message menus on other platforms", async () => {
  state.mac = false;
  const image = await openImage();
  expect(image.classList.contains("select-text")).toBe(false);
  image.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
  expect(ancestorMenu).not.toHaveBeenCalled();
});

it("does not dismiss when image selection ends on the backdrop; a backdrop click still closes", async () => {
  const image = await openImage();
  const dialog = image.closest('[role="dialog"]')!;
  await act(async () => {
    image.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    dialog.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  expect(
    host.querySelector('[data-testid="file-image-lightbox"]'),
  ).not.toBeNull();
  await act(async () => {
    dialog.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    dialog.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  expect(host.querySelector('[data-testid="file-image-lightbox"]')).toBeNull();
});

it("zooms, resets and restores focus when the preview closes", async () => {
  const image = await openImage();
  const trigger = host.querySelector<HTMLButtonElement>(
    'button[aria-label="files.viewFullSize"]',
  )!;
  const zoomIn = host.querySelector<HTMLButtonElement>(
    'button[aria-label="files.zoomIn"]',
  );
  expect(zoomIn).not.toBeNull();
  await act(async () => zoomIn!.click());
  expect(
    image.closest('[data-testid="image-transform"]')?.getAttribute("style"),
  ).toContain("scale(1.25)");
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        'button[aria-label="files.fitToWindow"]',
      )!
      .click(),
  );
  expect(
    image.closest('[data-testid="image-transform"]')?.getAttribute("style"),
  ).toContain("scale(1)");
  await act(async () =>
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  expect(host.querySelector('[data-testid="file-image-lightbox"]')).toBeNull();
  expect(document.activeElement).toBe(trigger);
});

it("anchors wheel zoom, clamps dragging and resets on double click", async () => {
  const image = await openImage();
  const transform = image.closest<HTMLElement>(
    '[data-testid="image-transform"]',
  )!;
  const stage = transform.parentElement!;
  Object.defineProperties(stage, {
    clientWidth: { value: 400 },
    clientHeight: { value: 300 },
  });
  Object.defineProperties(image, {
    offsetWidth: { value: 400 },
    offsetHeight: { value: 300 },
  });
  vi.spyOn(stage, "getBoundingClientRect").mockReturnValue({
    left: 0,
    top: 0,
    width: 400,
    height: 300,
  } as DOMRect);
  const wheel = new WheelEvent("wheel", {
    deltaY: -100,
    clientX: 300,
    clientY: 150,
    cancelable: true,
    bubbles: true,
  });
  await act(async () => stage.dispatchEvent(wheel));
  expect(wheel.defaultPrevented).toBe(true);
  expect(transform.style.transform).toMatch(/translate\(-22\.140/);
  stage.setPointerCapture = vi.fn();
  stage.hasPointerCapture = vi.fn(() => true);
  stage.releasePointerCapture = vi.fn();
  const pointer = (type: string, x: number) => {
    const event = new Event(type, { bubbles: true, cancelable: true });
    Object.assign(event, {
      pointerId: 1,
      clientX: x,
      clientY: 150,
      button: 0,
      isPrimary: true,
    });
    return event;
  };
  await act(async () => stage.dispatchEvent(pointer("pointerdown", 300)));
  await act(async () => stage.dispatchEvent(pointer("pointermove", 10000)));
  expect(transform.style.transform).toMatch(/translate\(44\.280/);
  await act(async () => stage.dispatchEvent(pointer("pointercancel", 10000)));
  const stopped = transform.style.transform;
  await act(async () => stage.dispatchEvent(pointer("pointermove", 0)));
  expect(transform.style.transform).toBe(stopped);
  expect(
    host.querySelector('[data-testid="file-image-lightbox"]'),
  ).not.toBeNull();
  await act(async () =>
    image.dispatchEvent(new MouseEvent("dblclick", { bubbles: true })),
  );
  expect(transform.style.transform).toBe("translate(0px, 0px) scale(1)");
});
