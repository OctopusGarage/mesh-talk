import { expect, it } from "vitest";
import { archivePack } from "./packArchive";
import { parsePack, type CustomizationPack } from "./pack";
const url = `data:image/png;base64,${btoa(String.fromCharCode(137, 80, 78, 71, 13, 10, 26, 10, 0))}`;
const base = {
  format: 1 as const,
  id: "saved.library",
  version: "1.0.0",
  name: "Saved",
};
it.each<CustomizationPack>([
  {
    ...base,
    kind: "avatar",
    category: "personal",
    fit: "cover",
    avatars: [{ label: "Face", url }],
  },
  {
    ...base,
    kind: "theme",
    base: "dark",
    colors: { background: "0 0% 0%" },
    wallpaper: url,
    crest: url,
    wallpapers: [{ id: "first", title: "First", url }],
  },
  {
    ...base,
    kind: "sticker",
    stickers: [{ id: "smile", label: "Smile", fallback: "🙂", url }],
  },
])("round trips a saved $kind library and all image references", (pack) => {
  expect(parsePack(archivePack(pack))).toEqual(pack);
});
it("rejects network images and invalid encoded image data without fetching", () => {
  const pack: CustomizationPack = {
    ...base,
    kind: "avatar",
    category: "personal",
    fit: "cover",
    avatars: [{ label: "Face", url: "https://example.test/private.png" }],
  };
  expect(() => archivePack(pack)).toThrow(/image/i);
  pack.avatars[0].url = "data:image/svg+xml;base64,PHN2Zy8+";
  expect(() => archivePack(pack)).toThrow(/image/i);
  pack.avatars[0].url = "data:image/png;base64,%%%";
  expect(() => archivePack(pack)).toThrow(/image/i);
});
