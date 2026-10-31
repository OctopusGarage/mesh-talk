import assert from "node:assert/strict";
import { test } from "node:test";
import { requiredBrowserPaths } from "../evals/browser-report.mjs";

const scenario = (path, id) => {
  const root = { title: path[0] };
  let parent = root;
  for (const title of path.slice(1, -1)) { const child = { title }; parent.suites = [child]; parent = child; }
  parent.specs = [{ id, title: path.at(-1), ok: true, tests: [{ projectName: "chromium", status: "expected", results: [{ status: "passed", attachments: ["portable-browser-evidence", "portable-browser-screenshot", "trace"].map(name => ({ name, path: `/case-${id}/${name}` })) }] }] }];
  return root;
};
const full = paths => ({ stats: { expected: paths.length + 1, unexpected: 0, flaky: 0, skipped: 3 }, errors: [], suites: [...paths.map((p, i) => scenario(p, String(i))), scenario(["other.spec.ts", "unrelated"], "other")] });

test("full Chromium results select the authoritative portable cases without accepting unrelated skips", async () => {
  const { selectPortableReport } = await import("../evals/chromium-evidence.mjs");
  const paths = requiredBrowserPaths("core").slice(0, 3);
  const selected = selectPortableReport(full(paths));
  assert.equal(selected.stats.expected, 3);
  assert.equal(selected.stats.skipped, 0);
  assert.deepEqual(selected.paths, paths);
  const duplicate = full(paths); duplicate.suites.push(structuredClone(duplicate.suites[0]));
  assert.throws(() => selectPortableReport(duplicate), /Duplicate/);
  for (const change of [r => r.errors.push({ message: "crashed" }), r => { r.suites[0].specs[0].tests[0].results[0].status = "skipped"; }, r => { r.suites[0].specs[0].tests[0].results[0].attachments.pop(); }]) {
    const broken = full(paths); change(broken);
    assert.throws(() => selectPortableReport(broken));
  }
});

test("portable shard collection requires exact source, run attempt, OS and complete unique inventory", async () => {
  const { validateShardInventory } = await import("../evals/chromium-evidence.mjs");
  const paths = requiredBrowserPaths("core");
  const identity = { sourceSha: "a".repeat(40), runId: "17", runAttempt: "2", platform: "win32" };
  const manifests = [1, 2, 3, 4].map(shard => ({ schema: 1, ...identity, shard, total: 4, paths: paths.filter((_, i) => i % 4 === shard - 1) }));
  assert.doesNotThrow(() => validateShardInventory(manifests, identity));
  for (const field of ["sourceSha", "runId", "runAttempt", "platform", "total", "shard"]) {
    const bad = structuredClone(manifests); bad[0][field] = field === "total" ? 3 : field === "shard" ? 2 : "wrong";
    assert.throws(() => validateShardInventory(bad, identity));
  }
  assert.throws(() => validateShardInventory(manifests.slice(1), identity));
  const missing = structuredClone(manifests); missing[0].paths.pop();
  assert.throws(() => validateShardInventory(missing, identity));
  const duplicate = structuredClone(manifests); duplicate[1].paths.push(duplicate[0].paths[0]);
  assert.throws(() => validateShardInventory(duplicate, identity));
});

test("real shard files survive relocation and revalidation rejects changed evidence bytes", async t => {
  const { mkdtemp, mkdir, writeFile, readFile, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join, resolve } = await import("node:path");
  const { pngFixture } = await import("./test-fixtures/png.mjs");
  const { traceFixture } = await import("./test-fixtures/trace.mjs");
  const { extractShard, collectShards } = await import("../evals/chromium-evidence.mjs");
  const root = await mkdtemp(join(tmpdir(), "mesh-chromium-evidence-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const identity = { sourceSha: "a".repeat(40), runId: "17", runAttempt: "2", platform: "win32" };
  const paths = requiredBrowserPaths("core");
  const all = join(root, "collected");
  for (let shard = 1; shard <= 4; shard++) {
    const selected = paths.filter((_, i) => i % 4 === shard - 1), report = full(selected);
    const raw = join(root, `raw-${shard}`);
    await mkdir(raw);
    for (let i = 0; i < selected.length; i++) {
      let suite = report.suites[i];
      while (suite.suites) suite = suite.suites[0];
      const attachments = suite.specs[0].tests[0].results[0].attachments;
      const evidence = { ...identity, sourceSha: identity.sourceSha, browser: "chromium", native: false, mocked: true, status: "passed", scenarioPath: selected[i], scenario: selected[i].at(-1), pageErrors: [], viewport: { width: 64, height: 64 } };
      for (const attachment of attachments) {
        attachment.path = resolve(raw, `${i}-${attachment.name}`);
        await writeFile(attachment.path, attachment.name === "portable-browser-evidence" ? JSON.stringify(evidence) : attachment.name === "trace" ? traceFixture() : pngFixture());
      }
    }
    const reportPath = join(raw, "results.json");
    await writeFile(reportPath, JSON.stringify(report));
    await extractShard(reportPath, join(all, String(shard)), shard, identity);
  }
  // Raw Playwright paths no longer exist: collection must use copied relative paths.
  for (let shard = 1; shard <= 4; shard++) await rm(join(root, `raw-${shard}`), { recursive: true });
  await collectShards(all, identity);
  const filename = join(all, "1/case-0-portable-browser-evidence");
  const evidence = JSON.parse(await readFile(filename, "utf8"));
  await writeFile(filename, JSON.stringify({ ...evidence, sourceSha: "b".repeat(40) }));
  await assert.rejects(collectShards(all, identity), /Stale browser evidence/);
  await writeFile(filename, JSON.stringify(evidence));
  await writeFile(join(all, "1/case-0-trace"), "not a ZIP");
  await assert.rejects(collectShards(all, identity), /Trace ZIP/);
});
