import { expect, it } from "vitest";
import { stickerPackId } from "./stickerPacks";
it("resolves legacy artwork owners without requesting unrelated packs", () => {
  expect(stickerPackId("cat-01-01", [])).toBe("cats");
  expect(stickerPackId("pack:custom.pack:wave", [])).toBe("custom.pack");
  expect(stickerPackId("unknown", [])).toBeUndefined();
});
