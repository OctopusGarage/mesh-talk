import { expect, it } from "vitest";
import { messageSummary, messageSticker } from "./messagePresentation";
import { encodeStickerMediaName } from "./stickerMedia";
import type { CustomizationPack } from "./pack";

const t = (key: string) =>
  ({
    "message.sticker": "[表情]",
    "message.recalledSummary": "[消息已撤回]",
    "protectedMessage.label": "Protected message",
    "messageFavorites.contentUnavailable": "Content unavailable",
  })[key] ?? key;
const file = (label?: string) => ({
  name: encodeStickerMediaName("pack:cats:calm", "🙂", "png", label),
  mime: "image/png",
  media: true,
  size: 42,
});
const packs = [
  {
    kind: "sticker",
    id: "cats",
    stickers: [
      {
        id: "calm",
        label: "New name",
        fallback: "🙂",
        url: "data:image/png;base64,AQID",
      },
    ],
  },
] as CustomizationPack[];

it("uses the sent semantic label without a pack and ignores subsequent catalog changes", () => {
  const message = { text: "", file: file("静下心") };
  expect(messageSummary(message, t)).toBe("[静下心]");
  expect(messageSummary(message, t, packs)).toBe("[静下心]");
  expect(messageSticker(message)?.label).toBe("静下心");
});
it("uses a legacy catalog label when available and a localized generic label otherwise", () => {
  expect(messageSummary({ file: file() }, t)).toBe("[表情]");
  expect(messageSummary({ file: file() }, t, packs)).toBe("[New name]");
  expect(
    messageSummary({ sticker: "pack:cats:calm", text: "🙂" }, t, packs),
  ).toBe("[New name]");
  expect(messageSummary({ sticker: "unknown", text: "🙂" }, t)).toBe("[表情]");
});
it("conceals protected, recalled and expired content before looking at attachment metadata", () => {
  const message = { text: "secret", file: file("静下心") };
  expect(messageSummary({ ...message, protection: "authenticate" }, t)).toBe(
    "Protected message",
  );
  expect(messageSummary({ ...message, recalled: true }, t)).toBe(
    "[消息已撤回]",
  );
  expect(messageSummary({ ...message, expiresAtMs: 99 }, t, [], 100)).toBe(
    "Content unavailable",
  );
  expect(messageSticker({ ...message, recalled: true })).toBeNull();
  expect(messageSticker({ ...message, protection: "authenticate" })).toBeNull();
});
it("preserves text and ordinary attachments without classifying malformed names as stickers", () => {
  expect(messageSummary({ text: "hello" }, t)).toBe("hello");
  expect(
    messageSummary(
      {
        file: {
          ...file(),
          name: "notes.txt",
          mime: "text/plain",
          media: false,
        },
      },
      t,
    ),
  ).toBe("notes.txt");
  expect(
    messageSticker({ file: { ...file(), name: "mt-sticker-v2-invalid.png" } }),
  ).toBeNull();
  expect(
    messageSummary(
      { file: { ...file(), name: "mt-sticker-v2-invalid.png" } },
      t,
    ),
  ).toBe("mt-sticker-v2-invalid.png");
});
