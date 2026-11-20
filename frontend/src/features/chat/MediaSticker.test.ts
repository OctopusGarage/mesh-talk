// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const state = vi.hoisted(() => ({
  current: true,
  url: null as string | null,
  status: undefined as
    | {
        ready: boolean;
        unavailable?: boolean;
        consent?: "pending" | "accepted" | "declined";
      }
    | undefined,
}));
vi.mock("react-i18next", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-i18next")>()),
  useTranslation: () => ({
    t: (key: string) => (key === "message.sticker" ? "[Sticker]" : key),
  }),
}));
vi.mock("@/store/chat", () => ({
  captureChatOwnership: () => ({ current: () => state.current }),
}));
vi.mock("@/store/fileAvailability", () => ({
  useFileStatus: () => state.status,
}));
vi.mock("./mediaFile", () => ({
  useFileObjectUrl: (_id: string, enabled: boolean) =>
    enabled ? state.url : null,
}));
import { MediaSticker } from "./MediaSticker";
let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  host = document.createElement("div");
  root = createRoot(host);
  state.current = true;
  state.url = null;
  state.status = undefined;
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
async function render(
  readOnly = false,
  label?: string,
  localPreview?: {
    url: string;
    current: () => boolean;
    release: () => void;
  },
) {
  await act(async () =>
    root.render(
      createElement(MediaSticker, {
        fileConv: "media",
        mime: "image/png",
        fallback: "👋",
        label,
        readOnly,
        localPreview,
      }),
    ),
  );
}
it("keeps the exact selected artwork until the durable sticker has decoded", async () => {
  const preview = {
    url: "blob:selected",
    current: () => true,
    release: vi.fn(),
  };
  await render(false, "Cat hug", preview);
  expect(host.querySelector('img[src="blob:selected"]')).not.toBeNull();
  expect(host.textContent).not.toContain("👋");
  state.status = { ready: true };
  state.url = "blob:received";
  await render(false, "Cat hug", preview);
  const durable = host.querySelector<HTMLImageElement>(
    'img[src="blob:received"]',
  )!;
  expect(durable).not.toBeNull();
  expect(preview.release).not.toHaveBeenCalled();
  Object.defineProperties(durable, {
    naturalWidth: { value: 128 },
    naturalHeight: { value: 128 },
  });
  await act(async () => durable.dispatchEvent(new Event("load")));
  expect(preview.release).toHaveBeenCalledTimes(1);
  expect(host.querySelector('img[src="blob:selected"]')).toBeNull();
});

it("never displays a selected preview after its owner retires", async () => {
  await render(false, "Cat hug", {
    url: "blob:retired",
    current: () => false,
    release: vi.fn(),
  });
  expect(host.querySelector('img[src="blob:retired"]')).toBeNull();
});
it("shows the fallback while waiting and the received image with accessible alt when ready", async () => {
  await render();
  expect(host.textContent).toContain("👋");
  expect(host.textContent).toContain("common.loading");
  expect(host.querySelector('[role="progressbar"]')).toBeNull();
  state.status = { ready: true };
  state.url = "blob:received";
  await render();
  expect(host.querySelector("img")?.getAttribute("src")).toBe("blob:received");
  expect(host.querySelector("img")?.getAttribute("alt")).toBe("[Sticker]");
  expect(host.querySelector("img")?.className).toContain("h-6 w-6");
});
it("keeps a semantic accessible name while media is missing and after it arrives", async () => {
  await render(false, "静下心");
  expect(host.querySelector('[role="img"]')?.getAttribute("aria-label")).toBe(
    "[静下心]",
  );
  state.status = { ready: true };
  state.url = "blob:received";
  await render(false, "静下心");
  expect(host.querySelector("img")?.getAttribute("alt")).toBe("[静下心]");
});
it("falls back on decode failure with retry and suppresses retry in read-only history", async () => {
  state.status = { ready: true };
  state.url = "blob:received";
  await render();
  await act(async () =>
    host.querySelector("img")!.dispatchEvent(new Event("error")),
  );
  expect(host.textContent).toContain("👋");
  expect(host.querySelector("button")?.textContent).toBe("common.retry");
  await act(async () => host.querySelector("button")!.click());
  expect(host.querySelector("img")).not.toBeNull();
  await act(async () =>
    host.querySelector("img")!.dispatchEvent(new Event("error")),
  );
  await render(true);
  expect(host.querySelector("button")).toBeNull();
});
it("hides a loaded image once retention makes the bytes unavailable", async () => {
  state.status = { ready: true };
  state.url = "blob:received";
  await render();
  state.status = { ready: false, unavailable: true };
  await render();
  expect(host.querySelector("img")).toBeNull();
  expect(host.textContent).toContain("files.unavailable");
});

it("rejects late sticker decode after source replacement and account retirement", async () => {
  const preview = {
    url: "blob:selected-late",
    current: () => true,
    release: vi.fn(),
  };
  state.status = { ready: true };
  state.url = "blob:old-sticker";
  await render(false, "Cat", preview);
  const old = host.querySelector<HTMLImageElement>(
    '[data-testid="message-sticker"]',
  )!;
  state.url = "blob:new-sticker";
  await render(false, "Cat", preview);
  Object.defineProperties(old, {
    naturalWidth: { value: 128 },
    naturalHeight: { value: 128 },
  });
  await act(async () => old.dispatchEvent(new Event("load")));
  expect(preview.release).not.toHaveBeenCalled();
  state.current = false;
  const next = host.querySelector<HTMLImageElement>(
    '[data-testid="message-sticker"]',
  )!;
  Object.defineProperties(next, {
    naturalWidth: { value: 128 },
    naturalHeight: { value: 128 },
  });
  await act(async () => next.dispatchEvent(new Event("load")));
  expect(preview.release).not.toHaveBeenCalled();
});
it("keeps selected sticker artwork actionable after durable decode failure", async () => {
  const preview = {
    url: "blob:selected-retry",
    current: () => true,
    release: vi.fn(),
  };
  state.status = { ready: true };
  state.url = "blob:failed-sticker";
  await render(false, "Cat", preview);
  await act(async () =>
    host
      .querySelector('[data-testid="message-sticker"]')!
      .dispatchEvent(new Event("error")),
  );
  expect(host.querySelector('img[src="blob:selected-retry"]')).not.toBeNull();
  expect(host.querySelector("button")?.textContent).toBe("common.retry");
  expect(preview.release).not.toHaveBeenCalled();
  await act(async () => host.querySelector("button")!.click());
  expect(host.querySelector('[data-testid="message-sticker"]')).not.toBeNull();
});

it("falls back safely when an optional selected sticker cannot decode", async () => {
  const preview = {
    url: "blob:selected-invalid",
    current: () => true,
    release: vi.fn(),
  };
  await render(false, "Cat", preview);
  await act(async () =>
    host
      .querySelector('img[src="blob:selected-invalid"]')!
      .dispatchEvent(new Event("error")),
  );
  expect(host.querySelector('img[src="blob:selected-invalid"]')).toBeNull();
  expect(host.textContent).toContain("👋");
  expect(preview.release).not.toHaveBeenCalled();
});

it("retires an old decoder error when the current source actually displays", async () => {
  state.status = { ready: true };
  state.url = "blob:old-error";
  await render();
  await act(async () =>
    host.querySelector("img")!.dispatchEvent(new Event("error")),
  );
  expect(host.textContent).toContain("files.imageUnsupported");
  state.url = "blob:recovered";
  await render();
  const img = host.querySelector<HTMLImageElement>(
    'img[src="blob:recovered"]',
  )!;
  Object.defineProperties(img, {
    naturalWidth: { value: 128 },
    naturalHeight: { value: 128 },
  });
  await act(async () => img.dispatchEvent(new Event("load")));
  expect(host.textContent).not.toContain("files.imageUnsupported");
  expect(host.textContent).not.toContain("common.retry");
});

it("keeps a pending sticker readable without file transfer controls", async () => {
  await act(async () =>
    root.render(
      createElement(MediaSticker, {
        fileConv: "",
        mime: "image/png",
        fallback: "👋",
      }),
    ),
  );
  expect(host.textContent).not.toContain("transfer.waiting");
});

it.each([
  ["pending", "files.approvalRequired"],
  ["declined", "files.declined"],
] as const)(
  "keeps sticker consent %s distinct from loading",
  async (consent, label) => {
    state.status = { ready: false, consent };
    await render();
    expect(host.textContent).toContain("👋");
    expect(host.textContent).toContain(label);
    expect(host.querySelector('[role="progressbar"]')).toBeNull();
    expect(host.querySelector("img")).toBeNull();
  },
);
