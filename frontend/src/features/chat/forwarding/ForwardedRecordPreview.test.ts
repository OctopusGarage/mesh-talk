// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const state = vi.hoisted(() => ({
  readMedia: vi.fn(),
  readFile: vi.fn(),
  save: vi.fn(),
  exportAttachment: vi.fn(),
  current: true,
}));
vi.mock("react-i18next", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-i18next")>()),
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/lib/api/chat", () => ({
  chat: { readMedia: state.readMedia, readFile: state.readFile },
}));
vi.mock("@/lib/api/messageForwarding", () => ({
  messageForwarding: { exportAttachment: state.exportAttachment },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: state.save }));
vi.mock("@/store/auth", () => ({
  useAuth: (selector: (value: unknown) => unknown) =>
    selector({ user: { id: "owner" }, generation: 1 }),
}));
vi.mock("@/store/chat", () => ({
  useChat: (selector: (value: unknown) => unknown) =>
    selector({ runEpoch: 1, identityEpoch: 1, ready: true }),
  captureChatOwnership: () => ({
    owner: "owner",
    current: () => state.current,
  }),
}));
vi.mock("@/store/fileAvailability", () => ({
  useFileStatus: () => ({ ready: true }),
}));
import { ForwardedRecordPreview } from "./ForwardedRecordPreview";
let root: Root, host: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("TextDecoder", globalThis.TextDecoder);
  state.current = true;
  state.save.mockReset().mockResolvedValue(null);
  state.exportAttachment.mockReset().mockResolvedValue("/tmp/export.bin");
  vi.stubGlobal(
    "URL",
    class extends URL {
      static createObjectURL = vi.fn(() => "blob:attachment");
      static revokeObjectURL = vi.fn();
    },
  );
  state.readMedia
    .mockReset()
    .mockRejectedValue(new Error("not retained media"));
  state.readFile.mockReset().mockResolvedValue(
    new TextEncoder().encode(
      JSON.stringify({
        format: "MTCHAT1",
        title: "History",
        created_at_ms: 1,
        messages: [
          {
            sender_name: "Alice",
            wall_clock: 1,
            text: "<script>literal text</script>",
            file: null,
          },
        ],
      }),
    ).buffer,
  );
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
async function open() {
  await act(async () =>
    root.render(
      createElement(ForwardedRecordPreview, {
        fileConv: "archive",
        name: "history.mtchat",
        size: 100,
        mime: "application/octet-stream",
      }),
    ),
  );
  await act(async () =>
    host.querySelector<HTMLButtonElement>("button")!.click(),
  );
}
it("uses durable-first fallback and renders historical text without HTML", async () => {
  await open();
  expect(state.readMedia).toHaveBeenCalledWith("archive");
  expect(state.readFile).toHaveBeenCalledWith("archive");
  expect(
    document.querySelector('[data-testid="forwarded-record-messages"]')
      ?.textContent,
  ).toContain("<script>literal text</script>");
  expect(
    document.querySelector('[data-testid="forwarded-record-messages"] script'),
  ).toBeNull();
});
it("shows an explicit decoding failure for corrupt records", async () => {
  state.readFile.mockResolvedValue(new TextEncoder().encode("{}").buffer);
  await open();
  expect(document.querySelector('[role="alert"]')?.textContent).toBe(
    "messageForwarding.recordInvalid",
  );
});
it("does not initiate fallback after the source session retires", async () => {
  state.readMedia.mockImplementation(async () => {
    state.current = false;
    throw new Error("retired");
  });
  await open();
  expect(state.readFile).not.toHaveBeenCalled();
  expect(
    document.querySelector('[data-testid="forwarded-record-messages"]'),
  ).toBeNull();
});

it("cancels attachment export without writing, then exports the exact message index", async () => {
  state.readFile.mockResolvedValue(
    new TextEncoder().encode(
      JSON.stringify({
        format: "MTCHAT1",
        title: "History",
        created_at_ms: 1,
        messages: [
          {
            sender_name: "Alice",
            wall_clock: 1,
            text: "",
            file: {
              name: "note.bin",
              mime: "application/octet-stream",
              size: 1,
              data_base64: "YQ==",
            },
          },
        ],
      }),
    ).buffer,
  );
  await open();
  const button = [
    ...document.querySelectorAll<HTMLButtonElement>("button"),
  ].find((button) => button.textContent === "messageForwarding.recordSave")!;
  await act(async () => button.click());
  expect(state.exportAttachment).not.toHaveBeenCalled();
  state.save.mockResolvedValue("/tmp/export.bin");
  await act(async () => button.click());
  expect(state.exportAttachment).toHaveBeenCalledWith(
    "owner",
    "archive",
    0,
    "/tmp/export.bin",
  );
});
