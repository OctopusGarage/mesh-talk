import { test } from "node:test";
import assert from "node:assert/strict";

import { pngFixture, chunk } from "./test-fixtures/png.mjs";

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

test("expected native receipt region rejects blank and gradient frames at screenshot scale", async () => {
  const inspectPng = await inspect();
  const region = { rect: { x: 20, y: 20, width: 20, height: 12 }, viewport: { width: 64, height: 64 } };
  for (const kind of ["blank", "gradient", "glyph"]) {
    const bytes = pngFixture({ width: 128, height: 128, pixel: (x,y) => {
      if (x >= 40 && x < 80 && y >= 40 && y < 64) {
        const glyph = kind === "glyph" && x % 8 < 3 && y > 44 && y < 60;
        const value = glyph ? 85 : kind === "gradient" ? 220 + Math.floor(x / 4) : 247;
        return [value,value,value,255];
      }
      return [x % 256,y % 256,(x+y)%256,255];
    } });
    const result = inspectPng(bytes, region);
    assert.equal(result.region.painted, kind === "glyph", kind);
    assert.deepEqual(result.region.bounds, { x: 40, y: 40, width: 40, height: 24 });
    assert.match(result.region.pixelDigest, /^[a-f0-9]{64}$/);
  }
  assert.throws(() => inspectPng(pngFixture(), { ...region, viewport: { width: 0, height: 64 } }), /viewport/i);
  assert.throws(() => inspectPng(pngFixture(), { ...region, rect: { x: 60, y: 20, width: 20, height: 12 } }), /region/i);
});
