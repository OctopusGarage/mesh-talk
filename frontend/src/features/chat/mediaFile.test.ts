import { describe, it, expect } from "vitest";
import { blobMime, isImage, isVideo, withinInlineCap } from "./mediaFile";

describe("blobMime", () => {
  it("types a video/image blob from the name even when the manifest MIME is generic", () => {
    // Regression: the manifest MIME was hardcoded "application/octet-stream", and the old
    // `mime || mimeFromName(name)` kept that (truthy) — so the inline <video> blob was typed
    // octet-stream and WKWebView silently refused to decode it (images byte-sniff, so they
    // were unaffected). The blob must be typed from the name for known media.
    expect(blobMime("clip.mp4", "application/octet-stream")).toBe("video/mp4");
    expect(blobMime("IMG_0001.MOV", "application/octet-stream")).toBe(
      "video/quicktime",
    );
    expect(blobMime("a.b.webm", undefined)).toBe("video/webm");
    expect(blobMime("photo.png", "application/octet-stream")).toBe("image/png");
  });

  it("falls back to the manifest MIME for unknown extensions", () => {
    expect(blobMime("notes.pdf", "application/pdf")).toBe("application/pdf");
    expect(blobMime("blob", undefined)).toBeUndefined();
  });

  it("classifies heic/heif consistently as images (picked == dropped)", () => {
    // Regression: the image-button dialog allowed heic/heif but isImage didn't, so the same
    // file previewed when picked yet became a generic attachment when dropped.
    expect(isImage("IMG_0001.HEIC")).toBe(true);
    expect(isImage("x.heif")).toBe(true);
    expect(blobMime("IMG_0001.heic", "application/octet-stream")).toBe(
      "image/heic",
    );
  });
});

describe("isVideo / withinInlineCap", () => {
  it("recognizes video extensions case-insensitively", () => {
    expect(isVideo("v.mp4")).toBe(true);
    expect(isVideo("V.MOV")).toBe(true);
    expect(isVideo("photo.png")).toBe(false);
  });
  it("gates inline rendering by size per kind", () => {
    expect(withinInlineCap("v.mp4", 50 * 1024 * 1024)).toBe(true);
    expect(withinInlineCap("v.mp4", 50 * 1024 * 1024 + 1)).toBe(false);
    expect(withinInlineCap("p.png", 16 * 1024 * 1024)).toBe(true);
    expect(withinInlineCap("p.png", 16 * 1024 * 1024 + 1)).toBe(false);
  });
});
