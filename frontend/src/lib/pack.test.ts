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
    ).toThrow(/image/i);
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

  it("loads every published marketplace archive and matches its checksum", () => {
    const catalog = JSON.parse(
      readFileSync(
        new URL("../../../site/market/catalog.json", import.meta.url),
        "utf8",
      ),
    ) as { id: string; file: string; sha256: string }[];
    expect(catalog).toHaveLength(11);
    for (const item of catalog) {
      const bytes = readFileSync(
        new URL(`../../../site/market/${item.file}`, import.meta.url),
      );
      expect(createHash("sha256").update(bytes).digest("hex"), item.id).toBe(
        item.sha256,
      );
      expect(parsePack(bytes).id).toBe(item.id);
    }
  });
});
