// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const state = vi.hoisted(() => ({
  url: null as string | null,
  status: undefined as { ready: boolean; unavailable?: boolean } | undefined,
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
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
  state.url = null;
  state.status = undefined;
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
async function render(readOnly = false) {
  await act(async () =>
    root.render(
      createElement(MediaSticker, {
        fileConv: "media",
        mime: "image/png",
        fallback: "👋",
        readOnly,
      }),
    ),
  );
}
it("shows the fallback while waiting and the received image with accessible alt when ready", async () => {
  await render();
  expect(host.textContent).toContain("👋");
  expect(host.textContent).toContain("transfer.waiting");
  state.status = { ready: true };
  state.url = "blob:received";
  await render();
  expect(host.querySelector("img")?.getAttribute("src")).toBe("blob:received");
  expect(host.querySelector("img")?.getAttribute("alt")).toBe("👋");
  expect(host.querySelector("img")?.className).toContain("h-32 w-32");
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
