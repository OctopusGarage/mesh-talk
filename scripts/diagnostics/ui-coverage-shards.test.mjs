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

const unitArtifact = new URL("../ci/unit-coverage-artifact.mjs", import.meta.url);

async function unitFixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), "mesh-unit-coverage-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const source = path.join(root, "unit.lcov");
  const artifact = path.join(root, "artifact");
  const output = path.join(root, "restored/lcov.info");
  const lcov = "SF:src/app.tsx\nDA:1,1\nDA:2,0\nend_of_record\n";
  await writeFile(source, lcov);
  const env = { ...process.env, GITHUB_SHA: sha, GITHUB_RUN_ID: "1234", GITHUB_RUN_ATTEMPT: "2", GITHUB_WORKFLOW: "UI E2E" };
  const run = (mode, overrides = {}) => spawnSync(process.execPath, [fileURLToPath(unitArtifact), mode, mode === "seal" ? source : artifact, mode === "seal" ? artifact : output], { env: { ...env, ...overrides }, encoding: "utf8" });
  return { root, source, artifact, output, lcov, run };
}

test("unit coverage artifact restores the exact line map for this source and workflow attempt", async t => {
  const f = await unitFixture(t);
  assert.equal(f.run("seal").status, 0);
  const restored = f.run("restore");
  assert.equal(restored.status, 0, restored.stderr);
  assert.equal(await readFile(f.output, "utf8"), f.lcov);
  // The existing browser merger still receives all coverable unit lines,
  // while browser hits (rather than unit hits) determine its numerator.
  await mkdir(path.join(f.root, "coverage/e2e-parts"), { recursive: true });
  await writeFile(path.join(f.root, "coverage/lcov.info"), await readFile(f.output));
  await writeFile(path.join(f.root, "coverage/e2e-parts/case.json"), JSON.stringify({ "http://localhost/src/app.tsx": [2] }));
  const merged = spawnSync(process.execPath, [fileURLToPath(merger)], { cwd: f.root, encoding: "utf8" });
  assert.equal(merged.status, 0, merged.stderr);
  assert.match(await readFile(path.join(f.root, "coverage/e2e.lcov"), "utf8"), /DA:1,0\nDA:2,1\nLF:2\nLH:1/);
});

for (const [name, overrides] of [
  ["revision", { GITHUB_SHA: "b".repeat(40) }],
  ["run", { GITHUB_RUN_ID: "5678" }],
  ["rerun attempt", { GITHUB_RUN_ATTEMPT: "3" }],
  ["workflow", { GITHUB_WORKFLOW: "Other workflow" }],
]) {
  test(`unit coverage artifact rejects the wrong ${name} before replacing output`, async t => {
    const f = await unitFixture(t);
    assert.equal(f.run("seal").status, 0);
    await mkdir(path.dirname(f.output), { recursive: true });
    await writeFile(f.output, "previous");
    const result = f.run("restore", overrides);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Coverage provenance mismatch/);
    assert.equal(await readFile(f.output, "utf8"), "previous");
  });
}

for (const [name, change] of [
  ["missing manifest", f => rm(path.join(f.artifact, "provenance.json"))],
  ["missing line map", f => rm(path.join(f.artifact, "lcov.info"))],
  ["tampered line map", f => writeFile(path.join(f.artifact, "lcov.info"), "SF:src/other.ts\nDA:1,1\nend_of_record\n")],
  ["unsupported manifest", f => writeFile(path.join(f.artifact, "provenance.json"), JSON.stringify({ schemaVersion: 999 }))],
]) {
  test(`unit coverage artifact fails closed for ${name}`, async t => {
    const f = await unitFixture(t);
    assert.equal(f.run("seal").status, 0);
    await change(f);
    assert.notEqual(f.run("restore").status, 0);
  });
}

test("unit coverage artifact cannot be sealed without complete identity or measured lines", async t => {
  const f = await unitFixture(t);
  assert.notEqual(f.run("seal", { GITHUB_RUN_ATTEMPT: "" }).status, 0);
  await writeFile(f.source, "");
  assert.notEqual(f.run("seal").status, 0);
});
