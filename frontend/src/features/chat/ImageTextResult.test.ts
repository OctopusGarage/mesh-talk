// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
const state = vi.hoisted(() => ({
  recognize: vi.fn(),
  current: true,
  copy: vi.fn().mockResolvedValue(true),
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/store/chat", () => ({
  captureChatOwnership: () => ({
    owner: "owner",
    current: () => state.current,
  }),
}));
vi.mock("@/lib/api", () => ({ chat: { recognizeImageText: state.recognize } }));
vi.mock("@/lib/sensitiveClipboard", () => ({ copySensitiveText: state.copy }));
import { ImageTextResult } from "./ImageTextResult";
(globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
it("shows extracted text and copies it through the privacy policy", async () => {
  state.recognize.mockResolvedValue("你好\nhello");
  state.current = true;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(
      createElement(ImageTextResult, { png: "png", onClose: () => {} }),
    );
  });
  expect(host.textContent).toContain("你好");
  await act(async () => {
    (host.querySelector("[data-copy-text]") as HTMLButtonElement).click();
  });
  expect(state.copy).toHaveBeenCalledWith(
    "你好\nhello",
    "settings.confirmCopyPrompt",
  );
  await act(async () => root.unmount());
  host.remove();
});
it("discards a completion after its preview unmounts", async () => {
  let finish!: (text: string) => void;
  state.recognize.mockReturnValue(
    new Promise<string>((resolve) => {
      finish = resolve;
    }),
  );
  const host = document.createElement("div");
  const root = createRoot(host);
  await act(async () =>
    root.render(
      createElement(ImageTextResult, { png: "png", onClose: () => {} }),
    ),
  );
  await act(async () => root.unmount());
  await act(async () => finish("retired private text"));
  expect(host.textContent).toBe("");
});
it("offers retry after failure and reports an empty result", async () => {
  state.current = true;
  state.recognize
    .mockRejectedValueOnce(new Error("busy"))
    .mockResolvedValueOnce("");
  const host = document.createElement("div");
  const root = createRoot(host);
  await act(async () =>
    root.render(
      createElement(ImageTextResult, { png: "png", onClose: () => {} }),
    ),
  );
  expect(host.textContent).toContain("files.textRecognitionFailed");
  const retry = Array.from(host.querySelectorAll("button")).find(
    (button) => button.textContent === "common.retry",
  )!;
  await act(async () => retry.click());
  expect(host.textContent).toContain("files.noTextFound");
  expect(host.querySelector("[data-copy-text]")).toBeNull();
  await act(async () => root.unmount());
});
it("does not publish results after ownership retirement", async () => {
  state.current = true;
  let finish!: (text: string) => void;
  state.recognize.mockReturnValue(
    new Promise<string>((resolve) => {
      finish = resolve;
    }),
  );
  const host = document.createElement("div");
  const root = createRoot(host);
  await act(async () =>
    root.render(
      createElement(ImageTextResult, { png: "png", onClose: () => {} }),
    ),
  );
  state.current = false;
  await act(async () => finish("private retired text"));
  expect(host.textContent).not.toContain("private retired text");
  await act(async () => root.unmount());
  state.current = true;
});
