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
