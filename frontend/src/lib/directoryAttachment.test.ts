import { expect, it } from "vitest";
import { attachmentLabel, DIRECTORY_MIME } from "./directoryAttachment";
import { encodeStickerMediaName } from "./stickerMedia";
it("labels valid media stickers semantically and preserves ordinary attachment names", () => {
  const name = encodeStickerMediaName("pack:cats:wave", "👋", "png");
  expect(attachmentLabel(name, "image/png", { media: true, size: 42 })).toBe(
    "[Sticker]",
  );
  expect(attachmentLabel(name, "image/png", { media: false, size: 42 })).toBe(
    name,
  );
  expect(
    attachmentLabel(name, "image/png", { media: true, size: 4 * 1024 * 1024 }),
  ).toBe(name);
  expect(attachmentLabel("photos.tar", DIRECTORY_MIME)).toBe("photos");
  expect(attachmentLabel("notes.txt", "text/plain")).toBe("notes.txt");
  const labelled = encodeStickerMediaName(
    "pack:cats:calm",
    "🙂",
    "png",
    "静下心",
  );
  expect(
    attachmentLabel(labelled, "image/png", { media: true, size: 42 }),
  ).toBe("[静下心]");
  expect(
    attachmentLabel(name, "image/png", { media: true, size: 42 }, "[表情]"),
  ).toBe("[表情]");
});
