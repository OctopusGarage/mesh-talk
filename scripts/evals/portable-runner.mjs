import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { copyFile, mkdir, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { resolve, dirname, relative, isAbsolute } from "node:path";
import { pathToFileURL } from "node:url";
import { requiredBrowserPaths, validateBrowserReport, validateBrowserEvidence } from "./browser-report.mjs";

export function portableShards(platform, scenarioCount = requiredBrowserPaths("core").length) {
  assert.ok(Number.isSafeInteger(scenarioCount) && scenarioCount > 0, "Missing portable scenario inventory");
  // Restart before Windows page-load latency accumulates. Keep at least the
  // existing four processes and derive extra capacity from the actual inventory.
  return platform === "win32" ? Math.min(scenarioCount, Math.max(4, Math.ceil(scenarioCount / 20))) : 1;
}

/** Preserve every outcome and identity; the full inventory remains authoritative. */
export function mergePortableReports(reports) {
  assert.ok(reports.length > 0, "Missing portable shard reports");
  return {
    stats: Object.fromEntries(["expected", "unexpected", "skipped", "flaky"].map(key => [key,
      reports.reduce((sum, report) => {
        assert.ok(Number.isSafeInteger(report.stats?.[key]) && report.stats[key] >= 0, `Invalid shard statistic: ${key}`);
        return sum + report.stats[key];
      }, 0),
    ])),
    errors: reports.flatMap(report => {
      assert.ok(Array.isArray(report.errors), "Missing shard harness errors");
      return report.errors;
    }),
    suites: reports.flatMap(report => {
      assert.ok(Array.isArray(report.suites), "Missing shard scenarios");
      return report.suites;
    }),
  };
}

export async function runPortable({ shards } = {}) {
  const browser = process.env.EVAL_BROWSER ?? "chromium", tier = process.env.EVAL_TIER ?? "core";
  assert.ok(["chromium", "webkit", "firefox"].includes(browser), "Unsupported portable browser");
  assert.ok(["core", "extended"].includes(tier), "Unsupported portable tier");
  const expectedPaths = requiredBrowserPaths(tier);
  const count = shards ?? portableShards(process.platform, expectedPaths.length);
  assert.ok(Number.isSafeInteger(count) && count > 0 && count <= expectedPaths.length, "Portable execution requires bounded nonempty shards");
  if (process.platform === "win32") assert.ok(Math.ceil(expectedPaths.length / count) <= 20, "Windows portable processes must contain at most 20 scenarios");
  const root = resolve(process.env.EVAL_EVIDENCE_ROOT ?? `eval-results/portable/${browser}/${tier}`);
  const reports = [];
  const cli = resolve("node_modules/@playwright/test/cli.js");
  for (let shard = 1; shard <= count; shard++) {
    const evidence = resolve(root, "shards", String(shard));
    const result = spawnSync(process.execPath, [cli, "test", "--config", "playwright.portable.config.ts", `--shard=${shard}/${count}`], {
      stdio: "inherit", env: { ...process.env, EVAL_EVIDENCE_ROOT: evidence },
    });
    if (result.error) throw result.error;
    assert.equal(result.status, 0, `Portable shard ${shard}/${count} did not pass`);
    reports.push(JSON.parse(await readFile(resolve(evidence, "results.json"), "utf8")));
  }
  const report = mergePortableReports(reports);
  validateBrowserReport(report, { count: expectedPaths.length, browser, expectedPaths });
  await mkdir(root, { recursive: true });
  await writeFile(resolve(root, "results.json"), JSON.stringify(report));
  console.log(`All ${expectedPaths.length} portable scenarios passed across ${count} isolated process(es); full evidence validation follows`);
}
function reportPaths(report) {
  const paths = [];
  const visit = (suite, parents = []) => {
    const path = [...parents, suite.title];
    for (const spec of suite.specs ?? []) paths.push([...path, spec.title]);
    for (const child of suite.suites ?? []) visit(child, path);
  };
  for (const suite of report.suites ?? []) visit(suite);
  return paths;
}
export function partitionIdentity(env = process.env) {
  for (const key of ["GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"]) assert.match(env[key] ?? "", /^\d+$/, `Missing ${key}`);
  assert.match(env.EVAL_SOURCE_SHA ?? "", /^[a-f0-9]{40}$/, "Missing source identity");
  assert.equal(env.EVAL_SOURCE_SHA, execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim(), "Source revision mismatch");
  assert.ok(["core", "extended"].includes(env.EVAL_TIER), "Missing portable tier");
  return { sourceSha: env.EVAL_SOURCE_SHA, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT,
    tier: env.EVAL_TIER, browser: "webkit", platform: "darwin" };
}
export function validatePartitions(manifests, identity) {
  assert.equal(manifests.length, 2, "Missing WebKit partitions");
  const ids = new Set();
  for (const item of manifests) {
    assert.equal(item.schema, 1); assert.equal(item.total, 2);
    assert.ok([1, 2].includes(item.shard) && !ids.has(item.shard), "Duplicate or invalid WebKit partition");
    ids.add(item.shard);
    for (const [key, value] of Object.entries(identity)) assert.equal(item[key], value, `Wrong WebKit partition ${key}`);
  }
}
export async function sealPartition(reportPath, output, shard, identity = partitionIdentity()) {
  assert.equal(identity.platform, process.platform, "WebKit producer differs from declared platform");
  assert.ok([1, 2].includes(shard), "Invalid WebKit partition");
  const report = JSON.parse(await readFile(reportPath, "utf8"));
  const paths = reportPaths(report);
  assert.ok(paths.length > 0);
  const allowed = new Set(requiredBrowserPaths(identity.tier).map(path => JSON.stringify(path)));
  assert.ok(paths.every(path => allowed.has(JSON.stringify(path))), "Unknown WebKit scenario");
  const results = validateBrowserReport(report, { count: paths.length, browser: "webkit", expectedPaths: paths });
  await validateBrowserEvidence(reportPath, { ...identity, expectedPaths: paths });
  await mkdir(output, { recursive: true });
  for (const [index, result] of results.entries()) for (const attachment of result.attachments) {
    if (!["portable-browser-evidence", "portable-browser-screenshot", "trace"].includes(attachment.name)) continue;
    const name = `case-${index}-${attachment.name}`;
    await copyFile(resolve(dirname(reportPath), attachment.path), resolve(output, name));
    attachment.path = name;
  }
  await writeFile(resolve(output, "results.json"), JSON.stringify(report));
  await writeFile(resolve(output, "partition.json"), JSON.stringify({ schema: 1, ...identity, shard, total: 2 }));
}
export async function collectPartitions(input, identity = partitionIdentity()) {
  input = await realpath(input);
  // A failed recollection must supersede any older union proof.
  await rm(resolve(input, "results.json"), { force: true });
  await rm(resolve(input, "validated-report.json"), { force: true });
  const manifests = [], reports = [];
  for (const name of await readdir(input)) {
    const directory = await realpath(resolve(input, name));
    const local = relative(await realpath(input), directory);
    assert.ok(local && !local.startsWith("..") && !isAbsolute(local), "WebKit partition escapes artifact directory");
    const manifest = JSON.parse(await readFile(resolve(directory, "partition.json"), "utf8"));
    manifests.push(manifest);
    const reportPath = resolve(directory, "results.json");
    const report = JSON.parse(await readFile(reportPath, "utf8"));
    await validateBrowserEvidence(reportPath, { ...identity, expectedPaths: reportPaths(report) });
    // Keep evidence inside the collected union, independent of producer paths.
    const rebase = suite => {
      for (const spec of suite.specs ?? []) for (const test of spec.tests ?? []) for (const result of test.results ?? []) {
        for (const attachment of result.attachments ?? []) if (attachment.path) attachment.path = relative(resolve(input), resolve(directory, attachment.path));
      }
      for (const child of suite.suites ?? []) rebase(child);
    };
    for (const suite of report.suites ?? []) rebase(suite);
    reports.push(report);
  }
  validatePartitions(manifests, identity);
  const merged = mergePortableReports(reports);
  validateBrowserReport(merged, { count: requiredBrowserPaths(identity.tier).length, browser: "webkit", expectedPaths: requiredBrowserPaths(identity.tier) });
  const output = resolve(input, "results.json");
  await writeFile(output, JSON.stringify(merged));
  await validateBrowserEvidence(output, identity);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [mode, input, output, shard] = process.argv.slice(2);
  if (mode === "seal-partition") await sealPartition(input, output, Number(shard));
  else if (mode === "collect-partitions") await collectPartitions(input);
  else { assert.equal(mode, undefined, "Unknown portable runner mode"); await runPortable(); }
}
