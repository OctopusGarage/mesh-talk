import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import i18n from "@/lib/i18n";
import { MessageBubble } from "./MessageBubble";
import type { ChatMessage } from "@/store/chat";

const base: ChatMessage = {
  id: "stable",
  clientId: "client",
  fromMe: true,
  who: "me",
  text: "text",
  wallClock: 1,
  replyTo: null,
  delivery: "awaiting",
};
const render = (m: ChatMessage, isChannel = false) =>
  renderToStaticMarkup(
    createElement(MessageBubble, {
      m,
      parent: null,
      showAuthor: false,
      isChannel,
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
  );

describe("automatic delivery footer", () => {
  it("renders accessible static awaiting/delivered for text, sticker and file cards", async () => {
    await i18n.changeLanguage("en");
    for (const m of [
      base,
      { ...base, sticker: "unknown", text: "🙂" },
      {
        ...base,
        metadataPending: true,
        file: {
          name: "file.png",
          size: 0,
          mime: "",
          fileConv: "",
          media: true,
        },
      },
    ]) {
      expect(render(m)).toContain('data-delivery="awaiting"');
      expect(render({ ...m, delivery: "delivered" })).toContain(
        'data-delivery="delivered"',
      );
      expect(render(m)).toContain('tabindex="0"');
    }
  });
  it("never shows a receipt claim for groups, incoming or untracked messages", () => {
    expect(render(base, true)).not.toContain("data-delivery=");
    expect(render({ ...base, fromMe: false })).not.toContain("data-delivery=");
    expect(render({ ...base, delivery: undefined })).not.toContain(
      "data-delivery=",
    );
  });
  it("has explicit automatic-receipt wording in all six locales and hides guessed file metadata", async () => {
    for (const language of ["en", "es", "ja", "yue", "zh-Hans", "zh-Hant"]) {
      for (const key of [
        "awaiting",
        "delivered",
        "help",
        "fileHelp",
        "metadataPending",
      ])
        expect(
          i18n.getResource(language, "translation", `message.delivery.${key}`),
        ).toBeTruthy();
      await i18n.changeLanguage(language);
      const markup = render({
        ...base,
        metadataPending: true,
        file: {
          name: "photo.png",
          size: 0,
          mime: "",
          fileConv: "",
          media: true,
        },
      });
      expect(markup).not.toContain("0 B");
      expect(markup).not.toContain('data-testid="file-image"');
      expect(markup).toContain('disabled=""');
    }
    await i18n.changeLanguage("en");
  });
});
