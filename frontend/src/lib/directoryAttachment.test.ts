import { expect, it } from "vitest";
import { attachmentLabel, DIRECTORY_MIME } from "./directoryAttachment";
import { encodeStickerMediaName } from "./stickerMedia";
it("labels bounded media stickers with their fallback instead of the encoded filename", () => {
  const name = encodeStickerMediaName("pack:cats:wave", "👋", "png");
  expect(attachmentLabel(name, "image/png", { media: true, size: 42 })).toBe(
    "👋",
  );
  expect(attachmentLabel(name, "image/png", { media: false, size: 42 })).toBe(
    name,
  );
  expect(
    attachmentLabel(name, "image/png", { media: true, size: 4 * 1024 * 1024 }),
  ).toBe(name);
  expect(attachmentLabel("photos.tar", DIRECTORY_MIME)).toBe("photos");
  expect(attachmentLabel("notes.txt", "text/plain")).toBe("notes.txt");
});
