import assert from "node:assert/strict";
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { stickerFixtures, validateStickerEvidence } from "./native-sticker-acceptance.mjs";
const require = createRequire(new URL("../../frontend/package.json", import.meta.url));
const { unzipSync } = require("fflate");

test("reviewed artwork fixtures stage separate scenario identities with byte-distinct replacement", async () => {
  const { original, replaced } = stickerFixtures(await readFile(new URL("../../site/market/packs/google-noto.zip", import.meta.url)));
  assert.notEqual(original.sha256, replaced.sha256);
  for (const [version, artwork] of [["1.0.0", original], ["2.0.0", replaced]]) {
    const files = unzipSync(artwork.zip), manifest = JSON.parse(new TextDecoder().decode(files["manifest.json"]));
    assert.equal(manifest.version, version);
    assert.deepEqual(manifest.stickers.map(sticker => sticker.id), ["missing", "installed", "replaced"]);
    assert.deepEqual(files[manifest.stickers[0].file], artwork.bytes);
    assert.ok(artwork.name.startsWith("mt-sticker-v1-"));
  }
});

function validReport() {
  const cases = Object.fromEntries(["missing", "installed", "replaced", "repeat"].map((kind, index) => [kind, {
    id: String(index + 1).repeat(64), fileConv: String(Math.min(index + 1, 3)).repeat(64), historyRows: 1,
    sha256: "a".repeat(64), decoded: true, width: 128, height: 128, delivered: true, captureSha256: "f".repeat(64),
  }]));
  return { native: true, mocked: false, buildVerified: true, sourceSha: "b".repeat(40), receiverRestarted: true,
    originalMediaPreserved: true, productionRegistrationAccepted: true, repeatTransferReused: true, originalSha256: "a".repeat(64), cases,
    limitations: ["Native older-peer and paired-device alias acceptance are not covered by this scenario."] };
}
test("native verdict rejects missing scenarios, stale reuse, duplicates, incorrect bytes and unverified builds", () => {
  validateStickerEvidence(validReport());
  for (const mutate of [
    r => { r.buildVerified = false; }, r => { r.mocked = true; },
    r => { delete r.cases.installed; }, r => { r.receiverRestarted = false; },
    r => { r.cases.installed.fileConv = r.cases.missing.fileConv; },
    r => { r.cases.repeat.id = r.cases.replaced.id; },
    r => { r.cases.missing.historyRows = 2; }, r => { r.cases.replaced.sha256 = "c".repeat(64); },
    r => { r.cases.installed.decoded = false; }, r => { r.cases.installed.width = 0; },
    r => { r.cases.repeat.delivered = false; }, r => { r.productionRegistrationAccepted = false; },
    r => { r.originalMediaPreserved = false; }, r => { delete r.cases.missing.captureSha256; },
  ]) { const report = validReport(); mutate(report); assert.throws(() => validateStickerEvidence(report)); }
});

test("native repeat accepts verified reuse or fresh post-prune transfer without aliasing another scenario", () => {
  const reused = validReport();
  reused.repeatTransferReused = true;
  validateStickerEvidence(reused);
  const restaged = validReport();
  restaged.cases.repeat.fileConv = "4".repeat(64);
  restaged.repeatTransferReused = false;
  validateStickerEvidence(restaged);
  restaged.repeatTransferReused = true;
  assert.throws(() => validateStickerEvidence(restaged));
  restaged.repeatTransferReused = false;
  restaged.cases.repeat.fileConv = restaged.cases.missing.fileConv;
  assert.throws(() => validateStickerEvidence(restaged));
});
