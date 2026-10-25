import { expect, it } from "vitest";
import {
  encodeStickerMediaName,
  decodeStickerMedia,
  snapshotSticker,
} from "./stickerMedia";

it("round trips a bounded canonical sticker marker on media only", () => {
  const name = encodeStickerMediaName("pack:cats:wave", "👋", "webp");
  expect(
    decodeStickerMedia({ name, media: true, size: 12, mime: "image/webp" }),
  ).toEqual({ id: "pack:cats:wave", fallback: "👋" });
  for (const change of [
    { media: false },
    { size: 3 * 1024 * 1024 + 1 },
    { mime: "image/png" },
    { size: 0 },
    { size: NaN },
  ])
    expect(
      decodeStickerMedia({
        name,
        media: true,
        size: 12,
        mime: "image/webp",
        ...change,
      }),
    ).toBeNull();
  expect(
    decodeStickerMedia({
      name: name.replace(".webp", "=.webp"),
      media: true,
      size: 12,
      mime: "image/webp",
    }),
  ).toBeNull();
});
it("rejects invalid ids, fallback controls and overlong fallback", () => {
  for (const [id, fallback] of [
    ["A", "🙂"],
    ["a".repeat(127), "🙂"],
    ["a", ""],
    ["a", "x\n"],
    ["a", "x".repeat(9)],
  ])
    expect(() => encodeStickerMediaName(id, fallback, "png")).toThrow();
});
it("snapshots installed data bytes with the source id and explicit MIME", () => {
  const snapshot = snapshotSticker(
    { id: "pack:cats:wave", emoji: "👋", url: "data:image/png;base64,AQID" },
    "👋",
  );
  expect(snapshot.bytes).toEqual(new Uint8Array([1, 2, 3]));
  expect(snapshot.mime).toBe("image/png");
  expect(
    decodeStickerMedia({
      ...snapshot,
      size: snapshot.bytes.length,
      media: true,
    })?.id,
  ).toBe("pack:cats:wave");
});
it("refuses remote, empty, noncanonical and oversized installed image snapshots", () => {
  for (const url of [
    "https://example.com/image.png",
    "data:image/png;base64,",
    "data:image/png;base64,AB==",
    `data:image/png;base64,${"AAAA".repeat(1024 * 1024 + 1)}`,
  ])
    expect(() =>
      snapshotSticker({ id: "cat-wave", emoji: "👋", url }, "👋"),
    ).toThrow();
});
