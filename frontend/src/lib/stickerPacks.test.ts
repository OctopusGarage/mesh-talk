import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { parsePack } from "./pack";
import { stickerById } from "./stickerPacks";

describe("older cat sticker messages", () => {
  it("uses installed cat artwork and keeps fallback when the pack is removed", () => {
    const pack = parsePack(
      readFileSync(
        new URL("../../../site/market/packs/cats.zip", import.meta.url),
      ),
    );
    expect(stickerById("cat-01-01", [pack])).toMatchObject({
      id: "cat-01-01",
      emoji: "😎",
      label: "Cool cat",
    });
    expect(stickerById("cat-01-01", [])).toBeUndefined();
  });
});

describe("older Noto sticker messages", () => {
  it("uses the removable Google Noto artwork and falls back after removal", () => {
    const pack = parsePack(
      readFileSync(
        new URL("../../../site/market/packs/google-noto.zip", import.meta.url),
      ),
    );
    expect(pack.kind).toBe("sticker");
    if (pack.kind !== "sticker") return;
    expect(pack.stickers).toHaveLength(40);
    for (const item of pack.stickers) {
      expect(stickerById(item.id, [pack])?.url).toBe(item.url);
    }
    expect(stickerById("1f602", [pack])).toMatchObject({
      id: "1f602",
      emoji: "😂",
    });
    expect(stickerById("pack:google-noto:1f602", [pack])).toMatchObject({
      emoji: "😂",
    });
    expect(stickerById("1f602", [])).toBeUndefined();
    expect(stickerById("pack:google-noto:1f602", [])).toBeUndefined();
  });
});
