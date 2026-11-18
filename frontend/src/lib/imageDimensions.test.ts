import { expect, it } from "vitest";
import { canPreviewStaticImage, imageDimensions } from "./imageDimensions";

function png(width: number, height: number) {
  const bytes = new Uint8Array(24);
  bytes.set([137, 80, 78, 71, 13, 10, 26, 10]);
  bytes.set([73, 72, 68, 82], 12);
  const view = new DataView(bytes.buffer);
  view.setUint32(16, width);
  view.setUint32(20, height);
  return bytes;
}
it("admits static PNG headers only within the native decoded-pixel ceiling", () => {
  expect(canPreviewStaticImage(png(4096, 4096), "image/png")).toBe(true);
  expect(canPreviewStaticImage(png(6000, 4000), "image/png")).toBe(false);
  expect(canPreviewStaticImage(png(0, 400), "image/png")).toBe(false);
  expect(canPreviewStaticImage(png(400, 0), "image/png")).toBe(false);
  expect(canPreviewStaticImage(png(0xffffffff, 0xffffffff), "image/png")).toBe(
    false,
  );
});
it("defers unknown, truncated and mislabeled images without constructing a decoder", () => {
  expect(canPreviewStaticImage(new Uint8Array([137, 80]), "image/png")).toBe(
    false,
  );
  const invalid = png(32, 32);
  invalid[0] = 0;
  expect(canPreviewStaticImage(invalid, "image/png")).toBe(false);
  expect(canPreviewStaticImage(png(32, 32), "image/jpeg")).toBe(false);
  expect(canPreviewStaticImage(png(32, 32), "image/webp")).toBe(false);
});
it("reads JPEG frame dimensions after metadata and rejects malformed lengths", () => {
  const bytes = new Uint8Array([
    0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 7, 8, 0, 32, 0, 64,
  ]);
  expect(imageDimensions(bytes, "image/jpeg")).toEqual({
    width: 64,
    height: 32,
  });
  expect(canPreviewStaticImage(bytes, "image/jpeg")).toBe(true);
  const huge = bytes.slice();
  huge[13] = 23;
  huge[14] = 112;
  huge[15] = 15;
  huge[16] = 160;
  expect(canPreviewStaticImage(huge, "image/jpeg")).toBe(false);
  expect(canPreviewStaticImage(bytes.subarray(0, 15), "image/jpeg")).toBe(
    false,
  );
  bytes[4] = 0;
  bytes[5] = 1;
  expect(canPreviewStaticImage(bytes, "image/jpeg")).toBe(false);
});
it("preserves GIF and WebP header inspection for pack validation", () => {
  const gif = new Uint8Array([71, 73, 70, 56, 57, 97, 32, 0, 64, 0]);
  expect(imageDimensions(gif, "image/gif")).toEqual({ width: 32, height: 64 });
  expect(canPreviewStaticImage(gif, "image/gif")).toBe(false);
  const webp = new Uint8Array(30);
  webp.set([86, 80, 56, 88], 12);
  webp[24] = 31;
  webp[27] = 63;
  expect(imageDimensions(webp, "image/webp")).toEqual({
    width: 32,
    height: 64,
  });
  expect(canPreviewStaticImage(webp, "image/webp")).toBe(false);
});

it("preserves lossless and lossy WebP geometry and rejects incomplete pack headers", () => {
  const lossy = new Uint8Array(30);
  lossy.set([86, 80, 56, 32], 12);
  lossy.set([0x9d, 1, 0x2a], 23);
  lossy[26] = 32;
  lossy[27] = 0xc0;
  lossy[28] = 64;
  lossy[29] = 0xc0;
  expect(imageDimensions(lossy, "image/webp")).toEqual({
    width: 32,
    height: 64,
  });
  const lossless = new Uint8Array(30);
  lossless.set([86, 80, 56, 76], 12);
  lossless[20] = 0x2f;
  lossless[21] = 31;
  lossless[22] = 192;
  lossless[23] = 15;
  expect(imageDimensions(lossless, "image/webp")).toEqual({
    width: 32,
    height: 64,
  });
  lossless[20] = 0;
  expect(imageDimensions(lossless, "image/webp")).toBeUndefined();
  for (const mime of [
    "image/png",
    "image/gif",
    "image/webp",
    "image/jpeg",
    "image/svg+xml",
  ])
    expect(imageDimensions(new Uint8Array(4), mime)).toBeUndefined();
});
it("defers JPEG end markers, truncated frames and malformed marker streams", () => {
  for (const bytes of [
    [255, 216, 255, 217, 0, 0],
    [255, 216, 255, 218, 0, 0],
    [255, 216, 0, 0, 0, 0],
    [255, 216, 255, 255, 255, 255],
    [255, 216, 255, 192, 0, 6, 8, 0, 32, 0],
  ])
    expect(
      imageDimensions(new Uint8Array(bytes), "image/jpeg"),
    ).toBeUndefined();
  const standalone = new Uint8Array([
    255, 216, 255, 1, 255, 255, 194, 0, 7, 8, 0, 32, 0, 64,
  ]);
  expect(imageDimensions(standalone, "image/jpeg")).toEqual({
    width: 64,
    height: 32,
  });
});
