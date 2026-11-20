import "@/lib/i18n";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import type { ChatMessage } from "@/store/chat";
import type { Transfer } from "@/store/transfers";
import { encodeStickerMediaName } from "@/lib/stickerMedia";

const transfer = vi.hoisted(() => ({
  value: undefined as Transfer | undefined,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/store/packs", () => ({
  usePacks: (select: (state: { packs: [] }) => unknown) =>
    select({ packs: [] }),
}));
vi.mock("@/store/transfers", () => ({
  useTransfer: () => transfer.value,
  useTransfers: (
    select: (state: {
      transfers: Record<string, Transfer | undefined>;
    }) => unknown,
  ) => select({ transfers: { queued: transfer.value } }),
}));
vi.mock("./DeliveryFooter", () => ({
  DeliveryFooter: () => createElement("span", { "data-message-footer": "" }),
}));
import { MessageBubble } from "./MessageBubble";

beforeEach(() => {
  transfer.value = undefined;
});
function render(m: Partial<ChatMessage>, readOnly = false) {
  return renderToStaticMarkup(
    createElement(MessageBubble, {
      m: {
        id: "queued",
        who: "me",
        text: "",
        wallClock: 1,
        fromMe: true,
        replyTo: null,
        ...m,
      },
      parent: null,
      showAuthor: false,
      isChannel: false,
      readOnly,
      reactions: [],
      selfReactionId: "me",
      myName: "Me",
      onReply: vi.fn(),
      onReact: vi.fn(),
      onRetry: vi.fn(),
      onDelete: vi.fn(),
      onRecall: vi.fn(),
      onReEdit: vi.fn(),
    }),
  );
}
const photo = {
  name: "photo.png",
  mime: "image/png",
  size: 8,
  fileConv: "file",
  media: true,
};
const sticker = {
  ...photo,
  name: encodeStickerMediaName("pack:test:wave", "👋", "png"),
};

it.each([photo, sticker])("keeps preparing media unobscured: $name", (file) => {
  const html = render({
    id: null,
    clientId: "preparing",
    pending: true,
    metadataPending: true,
    file,
  });
  expect(html).not.toContain("backdrop-blur-md");
  expect(html).not.toContain('role="progressbar"');
  expect(html).toContain("data-message-footer");
});
it.each([0, 4, 8])(
  "keeps file byte ticks out of photo and sticker content (%s bytes)",
  (done) => {
    transfer.value = { fileConv: "file", direction: "send", done, total: 8 };
    for (const file of [photo, sticker]) {
      const html = render({ file });
      expect(html).not.toContain("backdrop-blur-md");
      expect(html).not.toContain('role="progressbar"');
      expect(html).not.toContain("%<");
      expect(html).toContain("data-message-footer");
    }
  },
);
it.each([true, false])(
  "gives media and legacy stickers ordinary compact message geometry (fromMe=%s)",
  (fromMe) => {
    for (const m of [
      { file: sticker },
      { sticker: "legacy:missing", text: "👋" },
    ]) {
      const html = render({ ...m, fromMe });
      expect(html).toContain(fromMe ? "bg-bubble-own" : "bg-muted");
      expect(html).toContain("h-6 w-6");
      expect(html).not.toContain("h-32");
      expect(html).not.toContain("text-6xl");
    }
  },
);
it("keeps actual file transfer progress available", () => {
  transfer.value = { fileConv: "file", direction: "send", done: 4, total: 8 };
  const html = render({
    file: {
      ...photo,
      name: "document.pdf",
      mime: "application/pdf",
      media: false,
    },
  });
  expect(html).toContain('role="progressbar"');
  expect(html).toContain("50%");
});
it("keeps read-only media free of outgoing preparation controls", () => {
  expect(
    render({ file: photo, pending: true, metadataPending: true }, true),
  ).not.toContain("backdrop-blur-md");
});

it("announces a photo as image content rather than a file card", () => {
  const html = render({ file: photo });
  expect(html).toContain("[Photo]");
  expect(html).not.toContain("Sent a file");
});
