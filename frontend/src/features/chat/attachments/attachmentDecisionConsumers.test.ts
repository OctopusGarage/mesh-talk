// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import type { ChatMessage } from "@/store/chat";

const fixture = vi.hoisted(() => ({
  current: true,
  decide: vi.fn(),
  refresh: vi.fn(),
  setError: vi.fn(),
  statuses: { attachment: { consent: "pending", ready: false } },
  file: {
    fileConv: "attachment",
    name: "document.txt",
    mime: "text/plain",
    size: 12,
    media: false,
    fromName: "Alice",
  },
}));
vi.mock("@/lib/api", () => ({
  chat: { decideAttachment: fixture.decide },
  settings: {},
}));
vi.mock("@/store/chat", () => ({
  captureChatOwnership: () => ({ current: () => fixture.current }),
  useChat: (
    selector: (state: {
      incomingFiles: (typeof fixture.file)[];
      setError: typeof fixture.setError;
      dismissFile: () => void;
    }) => unknown,
  ) =>
    selector({
      incomingFiles: [fixture.file],
      setError: fixture.setError,
      dismissFile: () => {},
    }),
}));
vi.mock("@/store/fileAvailability", () => ({
  useFileStatus: () => fixture.statuses.attachment,
  useFileAvailability: (
    selector: (state: { statuses: typeof fixture.statuses }) => unknown,
  ) => selector({ statuses: fixture.statuses }),
  refreshFileAvailability: fixture.refresh,
  watchFileAvailability: () => () => {},
}));
vi.mock("@/lib/savedDownloads", () => ({ useSavedDownloads: () => ({}) }));
vi.mock("../TransferBar", () => ({ TransferBar: () => null }));
vi.mock("@/components/ui/dialog", () => {
  const Wrap = ({ children }: { children: ReactNode }) =>
    createElement("div", null, children);
  return {
    Dialog: Wrap,
    DialogContent: Wrap,
    DialogHeader: Wrap,
    DialogTitle: Wrap,
    DialogDescription: Wrap,
    DialogTrigger: Wrap,
  };
});
import { MessageBubble } from "../MessageBubble";
import { FilesTray } from "../FilesTray";

let host: HTMLDivElement;
let root: Root;
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  await i18n.changeLanguage("en");
  fixture.current = true;
  fixture.decide.mockReset().mockResolvedValue(undefined);
  fixture.refresh.mockReset().mockResolvedValue(undefined);
  fixture.setError.mockReset();
  host = document.createElement("div");
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
async function render(consumer: string) {
  await act(async () =>
    root.render(
      consumer === "tray"
        ? createElement(FilesTray)
        : createElement(MessageBubble, {
            m: {
              id: "message",
              fromMe: false,
              who: "Alice",
              text: "",
              wallClock: 1,
              replyTo: null,
              file: fixture.file,
            } as ChatMessage,
            parent: null,
            showAuthor: false,
            isChannel: false,
            reactions: [],
            selfReactionId: "me",
            myName: "Me",
            onReply: () => {},
            onReact: () => {},
            onRetry: () => {},
            onDelete: () => {},
            onRecall: () => {},
            onReEdit: () => {},
          }),
    ),
  );
}
async function click(accept: boolean) {
  const label = i18n.t(accept ? "files.approve" : "files.decline");
  const button = [...host.querySelectorAll("button")].find(
    (candidate) => candidate.textContent === label,
  )!;
  expect(button).toBeTruthy();
  await act(async () => button.click());
}
it.each(["bubble", "tray"])(
  "%s approval buttons submit distinct accept and decline decisions",
  async (consumer) => {
    await render(consumer);
    await click(true);
    await click(false);
    expect(fixture.decide.mock.calls).toEqual([
      ["attachment", true],
      ["attachment", false],
    ]);
    expect(fixture.refresh.mock.calls).toEqual([
      [["attachment"]],
      [["attachment"]],
    ]);
    expect(fixture.setError).not.toHaveBeenCalled();
  },
);
it.each(["bubble", "tray"])(
  "%s translates approval refresh errors locally",
  async (consumer) => {
    fixture.refresh.mockRejectedValue(new Error("availability failed"));
    await render(consumer);
    await click(true);
    expect(fixture.setError).toHaveBeenCalledWith(
      i18n.t("files.approvalFailed", { error: "availability failed" }),
    );
  },
);
it.each(["bubble", "tray"])(
  "%s ignores a stale native completion",
  async (consumer) => {
    let finish!: () => void;
    fixture.decide.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    await render(consumer);
    await click(true);
    fixture.current = false;
    await act(async () => finish());
    expect(fixture.refresh).not.toHaveBeenCalled();
    expect(fixture.setError).not.toHaveBeenCalled();
  },
);
