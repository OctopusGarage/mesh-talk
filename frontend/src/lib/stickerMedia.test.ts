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

it("snapshots a semantic label in a canonical v2 marker independently of the pack", () => {
  const snapshot = snapshotSticker(
    {
      id: "pack:cats:calm",
      emoji: "🙂",
      label: "静下心",
      url: "data:image/png;base64,AQID",
    },
    "🙂",
  );
  expect(snapshot.name).toBe(
    "mt-sticker-v2-cGFjazpjYXRzOmNhbG0K8J-ZggrpnZnkuIvlv4M.png",
  );
  expect(decodeStickerMedia({ ...snapshot, size: 3, media: true })).toEqual({
    id: "pack:cats:calm",
    fallback: "🙂",
    label: "静下心",
  });
});

it("fits long Unicode labels within a portable filename without splitting graphemes", () => {
  const snapshot = snapshotSticker(
    {
      id: "a".repeat(126),
      emoji: "🙂".repeat(8),
      label: "静下心".repeat(27),
      url: "data:image/png;base64,AQID",
    },
    "🙂".repeat(8),
  );
  expect(snapshot.name.length).toBeLessThanOrEqual(255);
  const metadata = decodeStickerMedia({ ...snapshot, size: 3, media: true });
  expect(metadata?.label).toBeTruthy();
  expect(metadata?.label).toMatch(/^[静下心]+$/);
});

it("removes label controls and bidi overrides before sending and rejects them on receipt", () => {
  const snapshot = snapshotSticker(
    {
      id: "calm",
      emoji: "🙂",
      label: "  静\n下\u202e心  ",
      url: "data:image/png;base64,AQID",
    },
    "🙂",
  );
  expect(decodeStickerMedia({ ...snapshot, size: 3, media: true })?.label).toBe(
    "静 下心",
  );
  for (const label of ["", " ", "evil\u202e", "a\nb", "a".repeat(81)]) {
    const name = `mt-sticker-v2-${btoa(
      unescape(encodeURIComponent(`calm\n🙂\n${label}`)),
    )
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "")}.png`;
    expect(
      decodeStickerMedia({ name, size: 3, media: true, mime: "image/png" }),
    ).toBeNull();
  }
});
