import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const collector = new URL("../ci/collect-ui-coverage.mjs", import.meta.url);
const merger = new URL("../../frontend/scripts/merge-e2e-coverage.mjs", import.meta.url);
const sha = "a".repeat(40);

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), "mesh-ui-shards-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const input = path.join(root, "artifacts");
  const output = path.join(root, "coverage/e2e-parts");
  for (let shard = 1; shard <= 4; shard++) {
    const dir = path.join(input, `ui-coverage-${shard}-2`);
    await mkdir(path.join(dir, "e2e-parts"), { recursive: true });
    await writeFile(path.join(dir, "shard.json"), JSON.stringify({ sourceSha: sha, shard, total: 4 }));
    // Identical filenames across runners must not overwrite one another.
    await writeFile(path.join(dir, "e2e-parts/case.json"), JSON.stringify({ "http://localhost/src/app.tsx": [shard] }));
  }
  const run = () => spawnSync(process.execPath, [fileURLToPath(collector), input, output, sha, "4"], { encoding: "utf8" });
  return { root, input, output, run };
}

test("all shard coverage survives collection and the real LCOV merge", async t => {
  const f = await fixture(t);
  await mkdir(f.output, { recursive: true });
  await writeFile(path.join(f.output, "stale.json"), "{}");
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
  assert.equal((await readdir(f.output)).length, 4);
  await writeFile(path.join(f.root, "coverage/lcov.info"), "SF:src/app.tsx\nDA:1,0\nDA:2,0\nDA:3,0\nDA:4,0\nend_of_record\n");
  const merge = spawnSync(process.execPath, [fileURLToPath(merger)], { cwd: f.root, encoding: "utf8" });
  assert.equal(merge.status, 0, merge.stderr);
  const lcov = await readFile(path.join(f.root, "coverage/e2e.lcov"), "utf8");
  for (let line = 1; line <= 4; line++) assert.ok(lcov.includes(`DA:${line},1\n`));
  assert.match(lcov, /LH:4/);
});

for (const [name, message, change] of [
  ["missing shard", /Missing browser coverage shards/, f => rm(path.join(f.input, "ui-coverage-4-2"), { recursive: true })],
  ["duplicate shard", /Invalid or duplicate coverage shard/, f => writeFile(path.join(f.input, "ui-coverage-4-2/shard.json"), JSON.stringify({ sourceSha: sha, shard: 3, total: 4 }))],
  ["stale revision", /Coverage belongs to another revision/, f => writeFile(path.join(f.input, "ui-coverage-4-2/shard.json"), JSON.stringify({ sourceSha: "b".repeat(40), shard: 4, total: 4 }))],
  ["wrong total", /Coverage shard count changed/, f => writeFile(path.join(f.input, "ui-coverage-4-2/shard.json"), JSON.stringify({ sourceSha: sha, shard: 4, total: 3 }))],
  ["empty coverage", /No browser coverage for shard/, f => rm(path.join(f.input, "ui-coverage-4-2/e2e-parts/case.json"))],
  ["invalid coverage map", /Invalid browser coverage map/, f => writeFile(path.join(f.input, "ui-coverage-4-2/e2e-parts/case.json"), "null")],
  ["invalid coverage lines", /Invalid browser coverage lines/, f => writeFile(path.join(f.input, "ui-coverage-4-2/e2e-parts/case.json"), JSON.stringify({ "http://localhost/src/app.tsx": [-1] }))],
  ["invalid coverage JSON", /SyntaxError/, f => writeFile(path.join(f.input, "ui-coverage-4-2/e2e-parts/case.json"), "{")],
]) {
  test(`collection rejects ${name} before replacing previous output`, async t => {
    const f = await fixture(t);
    await mkdir(f.output, { recursive: true });
    await writeFile(path.join(f.output, "previous.json"), "{}");
    await change(f);
    const result = f.run();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, message);
    assert.deepEqual(await readdir(f.output), ["previous.json"]);
  });
}
