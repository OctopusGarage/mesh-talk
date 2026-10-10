import { describe, expect, it } from "vitest";
import { strToU8, zipSync } from "fflate";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { parsePack } from "./pack";

const png = new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10, 0]);

function archive(manifest: unknown, files: Record<string, Uint8Array> = {}) {
  return zipSync({
    "manifest.json": strToU8(JSON.stringify(manifest)),
    ...files,
  });
}

describe("customization pack", () => {
  it("loads a personal avatar library from a ZIP", () => {
    const pack = parsePack(
      archive(
        {
          format: 1,
          id: "test.faces",
          version: "1.0.0",
          name: "Faces",
          kind: "avatar",
          category: "personal",
          fit: "cover",
          avatars: [{ label: "Ada", file: "images/ada.png" }],
        },
        { "images/ada.png": png },
      ),
    );
    expect(pack.kind).toBe("avatar");
    if (pack.kind === "avatar") {
      expect(pack.avatars[0].url).toMatch(/^data:image\/png;base64,/);
    }
  });

  it("rejects paths that escape the archive", () => {
    expect(() =>
      parsePack(
        archive(
          {
            format: 1,
            id: "test.faces",
            version: "1.0.0",
            name: "Faces",
            kind: "avatar",
            category: "personal",
            fit: "cover",
            avatars: [{ label: "Ada", file: "../ada.png" }],
          },
          { "../ada.png": png },
        ),
      ),
    ).toThrow(/path/i);
  });

  it("rejects an active content type disguised as an image", () => {
    expect(() =>
      parsePack(
        archive(
          {
            format: 1,
            id: "test.faces",
            version: "1.0.0",
            name: "Faces",
            kind: "avatar",
            category: "personal",
            fit: "cover",
            avatars: [{ label: "Ada", file: "images/ada.svg" }],
          },
          { "images/ada.svg": strToU8("<svg onload='alert(1)'>") },
        ),
      ),
    ).toThrow(/unsupported zip file/i);
  });

  it("rejects executable or unrelated files hidden in a pack", () => {
    expect(() =>
      parsePack(
        archive(
          {
            format: 1,
            id: "test.faces",
            version: "1.0.0",
            name: "Faces",
            kind: "avatar",
            category: "personal",
            fit: "cover",
            avatars: [{ label: "Ada", file: "images/ada.png" }],
          },
          { "images/ada.png": png, "setup.js": strToU8("alert(1)") },
        ),
      ),
    ).toThrow(/unsupported zip file/i);
  });

  it("rejects duplicate ZIP entry names", () => {
    const bytes = archive(
      {
        format: 1,
        id: "test.faces",
        version: "1.0.0",
        name: "Faces",
        kind: "avatar",
        category: "personal",
        fit: "cover",
        avatars: [{ label: "Ada", file: "images/a.png" }],
      },
      { "images/a.png": png, "images/b.png": png },
    );
    const original = strToU8("images/b.png");
    const duplicate = strToU8("images/a.png");
    let replacements = 0;
    for (let index = 0; index <= bytes.length - original.length; index++) {
      if (original.every((byte, offset) => bytes[index + offset] === byte)) {
        bytes.set(duplicate, index);
        replacements++;
      }
    }
    expect(replacements).toBe(2); // Local header and central directory.
    expect(() => parsePack(bytes)).toThrow(/duplicate ZIP entry/i);
  });

  it("rejects a ZIP with too many tiny entries", () => {
    const files = Object.fromEntries(
      Array.from({ length: 260 }, (_, index) => [
        `images/unused-${index}.png`,
        new Uint8Array(),
      ]),
    );
    expect(() =>
      parsePack(
        archive(
          {
            format: 1,
            id: "test.faces",
            version: "1.0.0",
            name: "Faces",
            kind: "avatar",
            category: "personal",
            fit: "cover",
            avatars: [{ label: "Ada", file: "images/ada.png" }],
          },
          { ...files, "images/ada.png": png },
        ),
      ),
    ).toThrow(/too many ZIP entries/i);
  });

  it("rejects unsafe theme token values", () => {
    expect(() =>
      parsePack(
        archive({
          format: 1,
          id: "test.theme",
          version: "1.0.0",
          name: "Unsafe",
          kind: "theme",
          base: "dark",
          colors: { primary: "red; background:url(x)" },
        }),
      ),
    ).toThrow(/color/i);
  });

  it("rejects theme colors outside the HSL range", () => {
    for (const value of ["361 50% 50%", "180 101% 50%", "180 50% 101%"]) {
      expect(() =>
        parsePack(
          archive({
            format: 1,
            id: "test.theme",
            version: "1.0.0",
            name: "Out of range",
            kind: "theme",
            base: "dark",
            colors: { primary: value },
          }),
        ),
      ).toThrow(/theme color/i);
    }
  });

  it("rejects duplicate wallpaper IDs that would select the wrong image", () => {
    expect(() =>
      parsePack(
        archive(
          {
            format: 1,
            id: "test.theme",
            version: "1.0.0",
            name: "Repeating walls",
            kind: "theme",
            base: "dark",
            colors: { primary: "180 50% 50%" },
            wallpapers: [
              { id: "same", title: "One", file: "images/a.png" },
              { id: "same", title: "Two", file: "images/b.png" },
            ],
          },
          { "images/a.png": png, "images/b.png": png },
        ),
      ),
    ).toThrow(/duplicate wallpaper id/i);
  });

  it("loads an animated sticker pack with an explicit fallback", () => {
    const gif = strToU8("GIF89a000000");
    const pack = parsePack(
      archive(
        {
          format: 1,
          id: "test.stickers",
          version: "1.0.0",
          name: "Reactions",
          kind: "sticker",
          stickers: [
            {
              id: "wave",
              label: "Wave",
              fallback: "👋",
              file: "images/wave.gif",
            },
          ],
        },
        { "images/wave.gif": gif },
      ),
    );
    expect(pack.kind).toBe("sticker");
    if (pack.kind === "sticker") {
      expect(pack.stickers[0].fallback).toBe("👋");
      expect(pack.stickers[0].url).toMatch(/^data:image\/gif;base64,/);
    }
  });

  it("rejects duplicate sticker IDs", () => {
    expect(() =>
      parsePack(
        archive(
          {
            format: 1,
            id: "test.stickers",
            version: "1.0.0",
            name: "Reactions",
            kind: "sticker",
            stickers: [
              {
                id: "wave",
                label: "Wave",
                fallback: "👋",
                file: "images/wave.png",
              },
              {
                id: "wave",
                label: "Wave again",
                fallback: "👋",
                file: "images/wave.png",
              },
            ],
          },
          { "images/wave.png": png },
        ),
      ),
    ).toThrow(/duplicate sticker id/i);
  });

  it("loads every published marketplace archive and matches its checksum", () => {
    const catalog = JSON.parse(
      readFileSync(
        new URL("../../../site/market/catalog.json", import.meta.url),
        "utf8",
      ),
    ) as {
      id: string;
      kind: string;
      version: string;
      file: string;
      sha256: string;
      preinstall: boolean;
    }[];
    expect(catalog).toHaveLength(15);
    expect(new Set(catalog.map((item) => item.id)).size).toBe(catalog.length);
    expect(catalog.filter((item) => item.kind !== "sticker")).toHaveLength(13);
    expect(catalog.filter((item) => item.preinstall)).toHaveLength(14);
    for (const item of catalog) {
      const bytes = readFileSync(
        new URL(`../../../site/market/${item.file}`, import.meta.url),
      );
      expect(createHash("sha256").update(bytes).digest("hex"), item.id).toBe(
        item.sha256,
      );
      const pack = parsePack(bytes);
      expect(pack.id).toBe(item.id);
      expect(pack.version).toBe(item.version);
    }
  });
});
