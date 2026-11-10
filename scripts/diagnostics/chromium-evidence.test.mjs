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

for (const platform of ["win32", "darwin"]) test(`real ${platform} shard files survive relocation and reject changed evidence`, async t => {
  const { mkdtemp, mkdir, writeFile, readFile, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join, resolve } = await import("node:path");
  const { pngFixture } = await import("./test-fixtures/png.mjs");
  const { traceFixture } = await import("./test-fixtures/trace.mjs");
  const { extractShard, collectShards } = await import("../evals/chromium-evidence.mjs");
  const root = await mkdtemp(join(tmpdir(), "mesh-chromium-evidence-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const identity = { sourceSha: "a".repeat(40), runId: "17", runAttempt: "2", platform };
  const { chromiumShardCount } = await import("../evals/chromium-evidence.mjs");
  const total = chromiumShardCount(platform);
  const paths = requiredBrowserPaths("core");
  const all = join(root, "collected");
  for (let shard = 1; shard <= total; shard++) {
    const selected = paths.filter((_, i) => i % total === shard - 1), report = full(selected);
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
  for (let shard = 1; shard <= total; shard++) await rm(join(root, `raw-${shard}`), { recursive: true });
  await collectShards(all, identity);
  const filename = join(all, "1/case-0-portable-browser-evidence");
  const evidence = JSON.parse(await readFile(filename, "utf8"));
  await writeFile(filename, JSON.stringify({ ...evidence, sourceSha: "b".repeat(40) }));
  await assert.rejects(collectShards(all, identity), /Stale browser evidence/);
  await writeFile(filename, JSON.stringify(evidence));
  await writeFile(join(all, "1/case-0-trace"), "not a ZIP");
  await assert.rejects(collectShards(all, identity), /Trace ZIP/);
});

test("platform shard layouts preserve exact partitions and reject invalid discovery", async () => {
  const { chromiumShardCount, validateShardLayout, validateShardInventory } = await import("../evals/chromium-evidence.mjs");
  for (const [os, platform, total] of [["ubuntu-24.04", "linux", 4], ["windows-latest", "win32", 4], ["macos-26-intel", "darwin", 8]]) {
    const shards = Array.from({ length: total }, (_, index) => index + 1);
    assert.equal(chromiumShardCount(os), total);
    assert.equal(chromiumShardCount(platform), total);
    assert.doesNotThrow(() => validateShardLayout(os, shards, total, total, platform));
    for (const bad of [shards.slice(1), [...shards, 1], shards.map(() => 1)]) assert.throws(() => validateShardLayout(os, bad, total, 1, platform));
    assert.throws(() => validateShardLayout(os, shards, total + 1, 1, platform));
    assert.throws(() => validateShardLayout(os, shards, total, total + 1, platform));
    assert.throws(() => validateShardLayout(os, shards, total, 1, "unknown"));
  }
  assert.throws(() => chromiumShardCount("unknown"));
  const identity = { sourceSha: "a".repeat(40), runId: "17", runAttempt: "2", platform: "darwin" };
  const paths = requiredBrowserPaths("core");
  const manifests = Array.from({ length: 8 }, (_, index) => ({ schema: 1, ...identity, shard: index + 1, total: 8, paths: paths.filter((_, item) => item % 8 === index) }));
  assert.doesNotThrow(() => validateShardInventory(manifests, identity));
  assert.throws(() => validateShardInventory(manifests.slice(0, 4), identity));
  const duplicate = structuredClone(manifests); duplicate[7].shard = 1;
  assert.throws(() => validateShardInventory(duplicate, identity));
});

test("downloaded collectors preserve attempt, OS partitions and diagnostic producer behavior", async () => {
  const { mkdtemp, mkdir, writeFile, rm, readdir } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { collectDownloadedShards, renderDownloadedReports, chromiumShardCount } = await import("../evals/chromium-evidence.mjs");
  const root = await mkdtemp(join(tmpdir(), "mesh-collected-ui-"));
  const systems = ["ubuntu-24.04", "macos-26-intel", "windows-latest"];
  try {
    for (const os of systems) for (let shard = 1; shard <= chromiumShardCount(os); shard++) {
      await mkdir(join(root, "portable", `chromium-portable-${os}-${shard}-2`), { recursive: true });
      const blob = join(root, "reports", `playwright-ui-${os}-${shard}-2`, "blob-report");
      await mkdir(blob, { recursive: true });
      await writeFile(join(blob, `report-${shard}.zip`), "fixture");
    }
    const identity = { sourceSha: "a".repeat(40), runId: "17", runAttempt: "2" };
    const collected = [];
    await collectDownloadedShards(join(root, "portable"), join(root, "collected"), identity, {
      collect: async (directory, actual) => collected.push({ files: await readdir(directory), actual }),
    });
    assert.deepEqual(collected.map(row => row.files.length), [4, 8, 4]);
    assert.deepEqual(collected.map(row => row.actual), ["linux", "darwin", "win32"].map(platform => ({ ...identity, platform })));
    const rendered = [];
    const options = { merge: async (_directory, os) => rendered.push(os), log() {} };
    await renderDownloadedReports(join(root, "reports"), join(root, "merged"), { runAttempt: "2", linuxResult: "failure", desktopResult: "cancelled", windowsResult: "failure" }, options);
    assert.deepEqual(rendered, systems, "failed admitted producers still yield diagnostic reports");
    rendered.length = 0;
    await renderDownloadedReports(join(root, "reports"), join(root, "skipped"), { runAttempt: "2", linuxResult: "success", desktopResult: "skipped", windowsResult: "skipped" }, options);
    assert.deepEqual(rendered, [systems[0]]);
    for (const [desktopResult, windowsResult, expected] of [["skipped", "success", [systems[0], systems[2]]], ["success", "skipped", systems.slice(0, 2)]]) {
      rendered.length = 0;
      await renderDownloadedReports(join(root, "reports"), join(root, "mixed"), {runAttempt:"2", linuxResult:"success", desktopResult, windowsResult}, options);
      assert.deepEqual(rendered, expected);
    }
    for (const windowsResult of [undefined, "", "unknown", "in_progress"]) await assert.rejects(renderDownloadedReports(join(root, "reports"), join(root, "invalid-windows"), {runAttempt:"2", linuxResult:"success", desktopResult:"success", windowsResult}, options), /incomplete/);
    for (const change of [{ runAttempt: "1" }, { linuxResult: "unknown" }, { linuxResult: "skipped", desktopResult: "skipped", windowsResult: "skipped" }]) {
      await assert.rejects(renderDownloadedReports(join(root, "reports"), join(root, "negative"), { runAttempt: "2", linuxResult: "success", desktopResult: "success", windowsResult: "success", ...change }, options), /incomplete/);
    }
    await rm(join(root, "reports", "playwright-ui-macos-26-intel-8-2"), { recursive: true });
    rendered.length = 0;
    await assert.rejects(renderDownloadedReports(join(root, "reports"), join(root, "missing"), { runAttempt: "2", linuxResult: "success", desktopResult: "success", windowsResult: "success" }, options));
    assert.deepEqual(rendered, [systems[0], systems[2]], "missing Mac evidence does not suppress available OS diagnostics");
    await assert.rejects(renderDownloadedReports(join(root, "reports"), join(root, "broken"), { runAttempt: "2", linuxResult: "success", desktopResult: "skipped", windowsResult: "skipped" }, { merge: async () => { throw Error("corrupt blob"); }, log() {} }));
  } finally { await rm(root, { recursive: true, force: true }); }
});
