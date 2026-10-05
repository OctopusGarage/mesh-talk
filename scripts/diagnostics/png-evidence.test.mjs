import { test } from "node:test";
import assert from "node:assert/strict";
import { deflateSync } from "node:zlib";

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  }
  return (crc ^ 0xffffffff) >>> 0;
}
function chunk(type, bytes) {
  const out = Buffer.alloc(12 + bytes.length);
  out.writeUInt32BE(bytes.length); out.write(type, 4); bytes.copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + bytes.length)), 8 + bytes.length);
  return out;
}
export function pngFixture({ width = 64, height = 64, solid = false, alpha = 255, filter = 0, malformedPixels = false } = {}) {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0); header.writeUInt32BE(height, 4);
  header[8] = 8; header[9] = 6;
  const rows = [];
  let previous = Buffer.alloc(width * 4);
  for (let y = 0; y < height; y++) {
    const raw = Buffer.alloc(width * 4);
    for (let x = 0; x < width; x++) {
      raw[x * 4] = solid ? 0 : (x * 7) % 256;
      raw[x * 4 + 1] = solid ? 0 : (y * 11) % 256;
      raw[x * 4 + 2] = solid ? 0 : (x + y) % 256;
      raw[x * 4 + 3] = alpha;
    }
    const row = Buffer.alloc(raw.length + 1); row[0] = filter;
    for (let i = 0; i < raw.length; i++) {
      const a = i >= 4 ? raw[i - 4] : 0, b = previous[i], c = i >= 4 ? previous[i - 4] : 0;
      const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
      const prediction = [0, a, b, Math.floor((a + b) / 2), pa <= pb && pa <= pc ? a : pb <= pc ? b : c][filter] ?? 0;
      row[i + 1] = (raw[i] - prediction) & 255;
    }
    rows.push(row); previous = raw;
  }
  const pixels = Buffer.concat(rows);
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", header), chunk("IDAT", deflateSync(malformedPixels ? pixels.subarray(0, 2) : pixels)), chunk("IEND", Buffer.alloc(0))]);
}
// Import in the test so missing implementation is an explicit feature failure.
async function inspect() { return (await import("../evals/png-evidence.mjs")).inspectPng; }
test("decodes all five PNG filters and proves screenshot pixels are not blank", async () => {
  const inspectPng = await inspect();
  for (let filter = 0; filter <= 4; filter++) {
    const result = inspectPng(pngFixture({ filter }));
    assert.equal(result.width, 64); assert.equal(result.height, 64);
    assert.equal(result.nonBlank, true); assert.ok(result.colorCount >= 8);
  }
});
test("rejects solid and invisible screenshots despite valid PNG structure", async () => {
  const inspectPng = await inspect();
  for (const bytes of [pngFixture({ solid: true }), pngFixture({ alpha: 0 })]) assert.throws(() => inspectPng(bytes), /blank|transparent/i);
});
test("rejects corrupt CRC, compressed pixels and truncated files", async () => {
  const inspectPng = await inspect();
  const damaged = pngFixture(); damaged[damaged.length - 1] ^= 1;
  for (const bytes of [damaged, pngFixture({ malformedPixels: true }), pngFixture().subarray(0, 100)]) assert.throws(() => inspectPng(bytes), /CRC|pixel|truncated|PNG/i);
});
test("rejects oversized declared dimensions before decompression", async () => {
  const inspectPng = await inspect();
  const bytes = pngFixture(); const header = Buffer.from(bytes.subarray(16, 29)); header.writeUInt32BE(100000, 0);
  const oversized = Buffer.concat([bytes.subarray(0, 8), chunk("IHDR", header), bytes.subarray(33)]);
  assert.throws(() => inspectPng(oversized), /dimension|size|limit/i);
});
