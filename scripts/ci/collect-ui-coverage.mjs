/** Collect every Linux shard before the existing browser LCOV merger runs. */
import assert from "node:assert/strict";
import { mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import path from "node:path";

const [input, output, sourceSha, shardCount] = process.argv.slice(2);
assert.ok(input && output, "Usage: collect-ui-coverage.mjs <artifacts> <output> <sha> <shards>");
assert.match(sourceSha ?? "", /^[a-f\d]{40}$/u, "Expected the current workflow source SHA");
const total = Number(shardCount);
assert.ok(Number.isSafeInteger(total) && total > 0, "Expected a positive shard count");
const shards = new Set();
const parts = [];
for (const directory of await readdir(input)) {
  const root = path.join(input, directory);
  const metadata = JSON.parse(await readFile(path.join(root, "shard.json"), "utf8"));
  assert.equal(metadata.sourceSha, sourceSha, "Coverage belongs to another revision");
  assert.equal(metadata.total, total, "Coverage shard count changed");
  const shard = metadata.shard;
  assert.ok(Number.isSafeInteger(shard) && shard >= 1 && shard <= total && !shards.has(shard), "Invalid or duplicate coverage shard");
  shards.add(shard);
  const files = (await readdir(path.join(root, "e2e-parts"))).filter(file => file.endsWith(".json")).sort();
  assert.ok(files.length > 0, `No browser coverage for shard ${shard}`);
  for (const [index, file] of files.entries()) {
    const part = JSON.parse(await readFile(path.join(root, "e2e-parts", file), "utf8"));
    assert.ok(part && typeof part === "object" && !Array.isArray(part), "Invalid browser coverage map");
    for (const lines of Object.values(part)) assert.ok(Array.isArray(lines) && lines.every(line => Number.isSafeInteger(line) && line > 0), "Invalid browser coverage lines");
    parts.push([`shard-${shard}-${index}.json`, JSON.stringify(part)]);
  }
}
assert.equal(shards.size, total, "Missing browser coverage shards");
// Validate the complete input before replacing output; never merge stale parts.
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
for (const [name, content] of parts) await writeFile(path.join(output, name), content);
console.log(`Collected ${parts.length} coverage parts from all ${total} shards for ${sourceSha}`);
