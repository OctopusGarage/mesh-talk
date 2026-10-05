// Bounded decoder for the RGB/RGBA screenshots emitted by the native drivers.
// This is screenshot evidence validation, not a general-purpose image viewer.
import assert from "node:assert/strict";
import { inflateSync } from "node:zlib";

const SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const LIMIT = 64 * 1024 * 1024;
const crcTable = Uint32Array.from({ length: 256 }, (_, value) => {
  let crc = value;
  for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  return crc >>> 0;
});
export function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) crc = (crc >>> 8) ^ crcTable[(crc ^ byte) & 255];
  return (crc ^ 0xffffffff) >>> 0;
}
function paeth(a, b, c) {
  const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
  return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
}
export function inspectPng(bytes) {
  assert.ok(Buffer.isBuffer(bytes) && bytes.length >= 33 && bytes.length <= LIMIT, "PNG size limit or missing data");
  assert.ok(bytes.subarray(0, 8).equals(SIGNATURE), "Invalid PNG signature");
  let width, height, channels, offset = 8, ended = false, dataEnded = false;
  const data = [];
  while (offset < bytes.length) {
    assert.ok(offset + 12 <= bytes.length, "Truncated PNG chunk");
    const length = bytes.readUInt32BE(offset);
    assert.ok(offset + 12 + length <= bytes.length, "Truncated PNG chunk data");
    const type = bytes.toString("ascii", offset + 4, offset + 8);
    const payload = bytes.subarray(offset + 8, offset + 8 + length);
    assert.equal(crc32(bytes.subarray(offset + 4, offset + 8 + length)), bytes.readUInt32BE(offset + 8 + length), `Invalid PNG CRC: ${type}`);
    if (offset === 8) assert.equal(type, "IHDR", "PNG must start with IHDR");
    if (type === "IHDR") {
      assert.ok(offset === 8 && length === 13, "Invalid or repeated PNG IHDR");
      width = payload.readUInt32BE(0); height = payload.readUInt32BE(4);
      assert.ok(width > 0 && height > 0 && width <= 8192 && height <= 8192 && width * height <= 16 * 1024 * 1024, "PNG dimension limit");
      assert.ok(payload[8] === 8 && [2, 6].includes(payload[9]) && payload[10] === 0 && payload[11] === 0 && payload[12] === 0, "Unsupported PNG screenshot encoding");
      channels = payload[9] === 6 ? 4 : 3;
      assert.ok(height * (width * channels + 1) <= LIMIT, "PNG decoded size limit");
    } else if (type === "IDAT") {
      assert.ok(!dataEnded, "Noncontiguous PNG pixel chunks");
      data.push(payload);
    } else if (type === "IEND") {
      assert.ok(length === 0 && data.length > 0 && offset + 12 === bytes.length, "Invalid PNG ending or missing pixels");
      ended = true;
    } else {
      assert.ok(/[a-z]/.test(type[0]) || type === "PLTE", `Unsupported critical PNG chunk: ${type}`);
      if (data.length) dataEnded = true;
    }
    offset += 12 + length;
  }
  assert.ok(ended, "Truncated PNG missing IEND");
  const stride = width * channels, expected = height * (stride + 1);
  const inflated = inflateSync(Buffer.concat(data), { maxOutputLength: expected });
  assert.equal(inflated.length, expected, "Incomplete PNG pixel data");
  let previous = Buffer.alloc(stride), samples = 0, opaque = 0;
  const colors = new Map();
  // Decode all pixels, sample a bounded, evenly distributed set for blankness.
  const sampleEvery = Math.max(1, Math.floor(width * height / 100000));
  for (let y = 0; y < height; y++) {
    const filter = inflated[y * (stride + 1)];
    assert.ok(filter <= 4, "Invalid PNG pixel filter");
    const row = Buffer.allocUnsafe(stride);
    for (let i = 0; i < stride; i++) {
      const a = i >= channels ? row[i - channels] : 0, b = previous[i], c = i >= channels ? previous[i - channels] : 0;
      const prediction = [0, a, b, Math.floor((a + b) / 2), paeth(a, b, c)][filter];
      row[i] = (inflated[y * (stride + 1) + 1 + i] + prediction) & 255;
    }
    for (let x = 0; x < width; x++) {
      if ((y * width + x) % sampleEvery) continue;
      samples++;
      const i = x * channels, alpha = channels === 4 ? row[i + 3] : 255;
      if (alpha < 128) continue;
      opaque++;
      const color = (row[i] << 16) | (row[i + 1] << 8) | row[i + 2];
      colors.set(color, (colors.get(color) ?? 0) + 1);
    }
    previous = row;
  }
  let dominant = 0;
  for (const count of colors.values()) dominant = Math.max(dominant, count);
  assert.ok(opaque > samples / 2 && colors.size >= 8 && dominant / opaque < 0.995, "Blank, nearly blank or transparent PNG screenshot");
  return { width, height, colorCount: colors.size, nonBlank: true };
}
