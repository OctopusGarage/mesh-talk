// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ChatMessage } from "@/store/chat";
const state = vi.hoisted(() => ({ ready: true, read: vi.fn() }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/store/packs", () => ({ usePacks: () => [] }));
vi.mock("@/store/fileAvailability", () => ({
  useFileStatus: () => ({ ready: state.ready }),
}));
vi.mock("@/lib/savedDownloads", () => ({ useSavedDownloads: () => ({}) }));
vi.mock("./mediaFile", async (original) => ({
  ...(await original<typeof import("./mediaFile")>()),
  useFileObjectUrl: (id: string, enabled: boolean) => {
    state.read(id, enabled);
    return enabled ? `blob:${id}` : null;
  },
}));
import { ReplyPreview } from "./ReplyPreview";
const photo: ChatMessage = {
  id: "photo",
  who: "alice",
  fromMe: false,
  text: "",
  wallClock: 1,
  replyTo: null,
  file: {
    fileConv: "photo-bytes",
    name: "image.png",
    mime: "image/png",
    size: 100,
    media: true,
  },
};
let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  state.ready = true;
  state.read.mockClear();
  host = document.createElement("div");
  root = createRoot(host);
});
afterEach(async () => {
  await act(() => root.unmount());
  vi.unstubAllGlobals();
});
async function render(message = photo) {
  await act(() => root.render(createElement(ReplyPreview, { message })));
}
it("shows recognizable photo content and replaces a failed image with a stable fallback", async () => {
  await render();
  expect(host.querySelector("img")?.getAttribute("src")).toBe(
    "blob:photo-bytes",
  );
  expect(host.textContent).toContain("message.image");
  await act(() => host.querySelector("img")!.dispatchEvent(new Event("error")));
  expect(host.querySelector("img")).toBeNull();
  expect(host.textContent).toContain("message.image");
});
it.each([
  { recalled: true },
  { protection: "authenticate" as const },
  { expiresAtMs: 1 },
  { pending: true },
  { file: { ...photo.file!, size: 17 * 1024 * 1024 } },
])("never loads hidden, pending, or oversized bytes: %j", async (patch) => {
  await render({ ...photo, ...patch });
  expect(state.read.mock.calls.every(([, enabled]) => !enabled)).toBe(true);
  expect(host.querySelector("img")).toBeNull();
});
it("keeps unavailable photos legible and recovers when availability changes", async () => {
  state.ready = false;
  await render();
  expect(host.textContent).toContain("files.unavailable");
  expect(host.querySelector("img")).toBeNull();
  state.ready = true;
  await render({ ...photo });
  expect(host.querySelector("img")).not.toBeNull();
});
it("preserves text summaries", async () => {
  await render({ ...photo, file: undefined, text: "A useful reference" });
  expect(host.textContent).toBe("A useful reference");
});

it("shows the original sender and thumbnail without visible filename clutter", async () => {
  await act(() =>
    root.render(
      createElement(ReplyPreview, { message: photo, authorName: "Alice" }),
    ),
  );
  expect(host.textContent).toBe("Alice:");
  expect(host.querySelector("img")?.alt).toBe("image.png");
});
