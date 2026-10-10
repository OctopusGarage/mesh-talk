import { test, expect } from "@playwright/test";
import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { strToU8, unzipSync, zipSync } from "fflate";
import { parsePack } from "../src/lib/pack";

const sampleZip = resolve("../site/market/packs/noto-favorites.zip");

test("validates a sticker ZIP locally and shows its pack details", async ({
  page,
}) => {
  const uploads: string[] = [];
  page.on("request", (request) => {
    if (request.method() !== "GET") uploads.push(request.url());
  });
  await page.goto("/studio/");
  await page.locator("#zip-input").setInputFiles(sampleZip);
  await expect(page.locator("#result-badge")).toHaveText("Passed");
  await expect(page.locator("#result-title")).toHaveText("Noto Favorites");
  await expect(page.locator("#result-facts")).toContainText("sticker");
  await expect(page.locator("#result-facts")).toContainText("8");
  await expect(page.locator("#preview img")).toHaveCount(8);
  expect(uploads).toEqual([]);
});

test("opens the ZIP picker from the keyboard", async ({ page }) => {
  await page.goto("/studio/");
  const choose = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: /Choose a pack ZIP/ }).focus();
  await page.keyboard.press("Enter");
  expect((await choose).isMultiple()).toBe(false);
});

test("rejects a ZIP with an image that cannot be decoded", async ({ page }) => {
  const bytes = zipSync({
    "manifest.json": strToU8(
      JSON.stringify({
        format: 1,
        id: "broken.image",
        version: "1.0.0",
        name: "Broken image",
        kind: "avatar",
        category: "personal",
        fit: "cover",
        avatars: [{ label: "Broken", file: "images/broken.png" }],
      }),
    ),
    "images/broken.png": new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10, 0]),
  });
  await page.goto("/studio/");
  await page.locator("#zip-input").setInputFiles({
    name: "broken.zip",
    mimeType: "application/zip",
    buffer: Buffer.from(bytes),
  });
  await expect(page.locator("#result-badge")).toHaveText("Failed");
  await expect(page.locator("#result-message")).toContainText(
    "cannot be decoded",
  );
});

test("keeps the latest result when an earlier image check finishes later", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const decode = Image.prototype.decode;
    let checks = 0;
    Image.prototype.decode = async function () {
      const pending = decode.call(this);
      if (++checks === 1)
        await new Promise((resolve) => setTimeout(resolve, 600));
      return pending;
    };
  });
  const image = unzipSync(readFileSync(sampleZip))["images/1f602.webp"];
  const makeZip = (name: string) =>
    Buffer.from(
      zipSync({
        "manifest.json": strToU8(
          JSON.stringify({
            format: 1,
            id: `test.${name.toLowerCase()}`,
            version: "1.0.0",
            name,
            kind: "avatar",
            category: "personal",
            fit: "cover",
            avatars: [{ label: name, file: "images/avatar.webp" }],
          }),
        ),
        "images/avatar.webp": image,
      }),
    );
  await page.goto("/studio/");
  await page.locator("#zip-input").setInputFiles({
    name: "slow.zip",
    mimeType: "application/zip",
    buffer: makeZip("Slow"),
  });
  await page.locator("#zip-input").setInputFiles({
    name: "fast.zip",
    mimeType: "application/zip",
    buffer: makeZip("Fast"),
  });
  await expect(page.locator("#result-title")).toHaveText("Fast");
  await expect(page.locator("#result-badge")).toHaveText("Passed");
  await page.waitForTimeout(750);
  await expect(page.locator("#result-title")).toHaveText("Fast");
});

test("downloads the creator toolkit with three examples and CLI sources", async ({
  page,
}) => {
  await page.goto("/studio/");
  const downloadPromise = page.waitForEvent("download");
  await page.getByRole("button", { name: /Download creator toolkit/ }).click();
  const download = await downloadPromise;
  expect(download.suggestedFilename()).toBe("mesh-talk-pack-toolkit.zip");
  const path = await download.path();
  if (!path) throw new Error("Toolkit download path missing");
  const entries = unzipSync(readFileSync(path));
  for (const kind of ["avatar", "theme", "sticker"]) {
    expect(entries[`examples/${kind}/manifest.json`]).toBeDefined();
    expect(entries[`examples/${kind}/images/sample.png`]).toBeDefined();
    const example = zipSync({
      "manifest.json": entries[`examples/${kind}/manifest.json`],
      "images/sample.png": entries[`examples/${kind}/images/sample.png`],
    });
    expect(parsePack(example).kind).toBe(kind);
  }
  expect(entries["scripts/pack.mjs"]).toBeDefined();
  expect(entries["src/lib/pack.ts"]).toBeDefined();
});

test("builds a ZIP from a selected source folder", async ({
  page,
}, testInfo) => {
  const source = testInfo.outputPath("studio-source");
  mkdirSync(resolve(source, "images"), { recursive: true });
  writeFileSync(
    resolve(source, "manifest.json"),
    JSON.stringify({
      format: 1,
      id: "test.studio",
      version: "1.0.0",
      name: "Studio test",
      kind: "sticker",
      stickers: [
        { id: "joy", label: "Joy", fallback: "😂", file: "images/joy.webp" },
      ],
    }),
  );
  const published = unzipSync(readFileSync(sampleZip));
  writeFileSync(
    resolve(source, "images/joy.webp"),
    published["images/1f602.webp"],
  );
  writeFileSync(
    resolve(source, "notes.txt"),
    "This file is not part of a pack",
  );
  await page.goto("/studio/");
  const downloadPromise = page.waitForEvent("download");
  await page.locator("#folder-input").setInputFiles(source);
  const download = await downloadPromise;
  expect(download.suggestedFilename()).toBe("test.studio.zip");
  await expect(page.locator("#result-title")).toHaveText("Studio test");
  await expect(page.locator("#result-message")).toContainText(
    "1 unrelated source file was left out",
  );
  const path = await download.path();
  if (!path) throw new Error("Built ZIP download path missing");
  const archive = unzipSync(readFileSync(path));
  expect(archive["images/joy.webp"]).toBeDefined();
  expect(archive["notes.txt"]).toBeUndefined();
  expect(
    JSON.parse(new TextDecoder().decode(archive["manifest.json"])).id,
  ).toBe("test.studio");
});
