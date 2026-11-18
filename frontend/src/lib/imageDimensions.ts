/** Bounded raster header inspection. Never creates a decoder or reads pixels. */
const ascii = (bytes: Uint8Array, start: number, end: number) =>
  String.fromCharCode(...bytes.subarray(start, end));

export function imageDimensions(
  bytes: Uint8Array,
  mime: string,
): { width: number; height: number } | undefined {
  const u16le = (at: number) => bytes[at] | (bytes[at + 1] << 8);
  const u16be = (at: number) => (bytes[at] << 8) | bytes[at + 1];
  const u32be = (at: number) =>
    (bytes[at] * 0x1000000 +
      (bytes[at + 1] << 16) +
      (bytes[at + 2] << 8) +
      bytes[at + 3]) >>>
    0;
  if (mime === "image/png") {
    if (bytes.length < 24 || ascii(bytes, 12, 16) !== "IHDR") return undefined;
    return { width: u32be(16), height: u32be(20) };
  }
  if (mime === "image/gif") {
    if (bytes.length < 10) return undefined;
    return { width: u16le(6), height: u16le(8) };
  }
  if (mime === "image/webp") {
    if (bytes.length < 30) return undefined;
    const kind = ascii(bytes, 12, 16);
    if (kind === "VP8X") {
      return {
        width: 1 + bytes[24] + (bytes[25] << 8) + (bytes[26] << 16),
        height: 1 + bytes[27] + (bytes[28] << 8) + (bytes[29] << 16),
      };
    }
    if (
      kind === "VP8 " &&
      bytes[23] === 0x9d &&
      bytes[24] === 0x01 &&
      bytes[25] === 0x2a
    ) {
      return { width: u16le(26) & 0x3fff, height: u16le(28) & 0x3fff };
    }
    if (kind === "VP8L" && bytes[20] === 0x2f) {
      return {
        width: 1 + bytes[21] + ((bytes[22] & 0x3f) << 8),
        height:
          1 + (bytes[22] >> 6) + (bytes[23] << 2) + ((bytes[24] & 0x0f) << 10),
      };
    }
    return undefined;
  }
  if (mime === "image/jpeg") {
    let at = 2;
    while (at + 3 < bytes.length) {
      if (bytes[at++] !== 0xff) return undefined;
      while (bytes[at] === 0xff) at++;
      const marker = bytes[at++];
      if (marker === 0xd9 || marker === 0xda) return undefined;
      if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd8)) continue;
      if (at + 2 > bytes.length) return undefined;
      const length = u16be(at);
      if (length < 2 || at + length > bytes.length) return undefined;
      if (
        (marker >= 0xc0 && marker <= 0xc3) ||
        (marker >= 0xc5 && marker <= 0xc7) ||
        (marker >= 0xc9 && marker <= 0xcb) ||
        (marker >= 0xcd && marker <= 0xcf)
      ) {
        if (length < 7) return undefined;
        return { width: u16be(at + 5), height: u16be(at + 3) };
      }
      at += length;
    }
  }
  return undefined;
}

/** Optional static outgoing previews obey the native inline pixel ceiling.
 * Unverified or oversized dimensions keep the ordinary preparing surface. */
export function canPreviewStaticImage(
  bytes: Uint8Array,
  mime: string,
): boolean {
  const valid =
    mime === "image/png"
      ? [137, 80, 78, 71, 13, 10, 26, 10].every(
          (byte, index) => bytes[index] === byte,
        )
      : mime === "image/jpeg" && bytes[0] === 0xff && bytes[1] === 0xd8;
  if (!valid) return false;
  const dimensions = imageDimensions(bytes, mime);
  return (
    !!dimensions &&
    dimensions.width > 0 &&
    dimensions.height > 0 &&
    dimensions.width * dimensions.height <= 16_777_216
  );
}
