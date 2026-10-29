/** Portable acceptance from the existing full Chromium execution; never re-runs tests. */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { copyFile, mkdir, readFile, readdir, realpath, writeFile } from "node:fs/promises";
import { dirname, resolve, relative, isAbsolute } from "node:path";
import { pathToFileURL } from "node:url";
import { requiredBrowserPaths, validateBrowserReport, validateBrowserEvidence } from "./browser-report.mjs";

export function selectPortableReport(report) {
  const required = new Set(requiredBrowserPaths("core").map(path => JSON.stringify(path)));
  const selected = { errors: report.errors, stats: { expected: 0, unexpected: 0, flaky: 0, skipped: 0 }, suites: [], paths: [] };
  function select(suite, parents = []) {
    const path = [...parents, suite.title];
    const specs = (suite.specs ?? []).filter(spec => required.has(JSON.stringify([...path, spec.title])));
    for (const spec of specs) selected.paths.push([...path, spec.title]);
    const suites = (suite.suites ?? []).map(child => select(child, path)).filter(Boolean);
    return specs.length || suites.length ? { ...suite, specs, suites } : null;
  }
  selected.suites = (report.suites ?? []).map(suite => select(suite)).filter(Boolean);
  selected.stats.expected = selected.paths.length;
  validateBrowserReport(selected, { count: selected.paths.length, browser: "chromium", expectedPaths: selected.paths });
  return selected;
}

export function validateShardInventory(manifests, identity) {
  assert.equal(manifests.length, 4, "Missing Chromium portable shards");
  const seen = new Set(), paths = [];
  for (const manifest of manifests) {
    assert.equal(manifest.schema, 1, "Unsupported portable shard schema");
    for (const field of ["sourceSha", "runId", "runAttempt", "platform"]) assert.equal(manifest[field], identity[field], `Wrong portable shard ${field}`);
    assert.equal(manifest.total, 4, "Unexpected portable shard total");
    assert.ok(Number.isInteger(manifest.shard) && manifest.shard >= 1 && manifest.shard <= 4 && !seen.has(manifest.shard), "Invalid or duplicate portable shard");
    seen.add(manifest.shard);
    paths.push(...manifest.paths.map(path => JSON.stringify(path)));
  }
  assert.deepEqual(paths.sort(), requiredBrowserPaths("core").map(path => JSON.stringify(path)).sort(), "Incomplete or duplicate portable scenario inventory");
}

function identity(platform = process.platform) {
  const sourceSha = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
  assert.match(sourceSha, /^[a-f0-9]{40}$/);
  assert.equal(process.env.GITHUB_SHA, sourceSha, "Checkout differs from workflow source");
  assert.match(process.env.GITHUB_RUN_ID ?? "", /^\d+$/);
  assert.match(process.env.GITHUB_RUN_ATTEMPT ?? "", /^\d+$/);
  return { sourceSha, runId: process.env.GITHUB_RUN_ID, runAttempt: process.env.GITHUB_RUN_ATTEMPT, platform };
}

export async function extractShard(reportPath, output, shard, expectedIdentity = identity()) {
  assert.ok(Number.isInteger(shard) && shard >= 1 && shard <= 4, "Invalid Chromium shard");
  reportPath = resolve(reportPath); output = resolve(output);
  const selected = selectPortableReport(JSON.parse(await readFile(reportPath, "utf8")));
  const results = validateBrowserReport(selected, { count: selected.paths.length, browser: "chromium", expectedPaths: selected.paths });
  await mkdir(output, { recursive: true });
  for (const [index, result] of results.entries()) {
    for (const attachment of result.attachments.filter(value => ["portable-browser-evidence", "portable-browser-screenshot", "trace"].includes(value.name))) {
      const filename = `case-${index}-${attachment.name}`;
      await copyFile(resolve(dirname(reportPath), attachment.path), resolve(output, filename));
      attachment.path = filename;
    }
  }
  const filename = resolve(output, "results.json");
  await writeFile(filename, JSON.stringify(selected));
  await validateBrowserEvidence(filename, { ...expectedIdentity, expectedPaths: selected.paths });
  const manifest = { schema: 1, ...expectedIdentity, shard, total: 4, paths: selected.paths };
  await writeFile(resolve(output, "shard.json"), JSON.stringify(manifest));
  console.log(`Validated ${selected.paths.length} portable cases in Chromium shard ${shard}/4`);
}

export async function collectShards(input, expectedIdentity) {
  input = await realpath(input);
  const manifests = [], roots = [];
  for (const name of await readdir(input)) {
    const root = await realpath(resolve(input, name));
    const local = relative(input, root);
    assert.ok(local && !local.startsWith("..") && !isAbsolute(local), "Portable shard escapes artifact directory");
    manifests.push(JSON.parse(await readFile(resolve(root, "shard.json"), "utf8"))); roots.push(root);
  }
  validateShardInventory(manifests, expectedIdentity);
  for (const [index, root] of roots.entries()) {
    await validateBrowserEvidence(resolve(root, "results.json"), { ...expectedIdentity, expectedPaths: manifests[index].paths });
  }
  console.log(`Validated all 76 ${expectedIdentity.platform} Chromium portable scenarios from four full-suite shards`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [mode, input, output] = process.argv.slice(2);
  if (mode === "extract") await extractShard(input, output, Number(process.env.CHROMIUM_SHARD));
  else if (mode === "collect") await collectShards(input, identity(output));
  else throw Error("usage: chromium-evidence.mjs extract REPORT OUTPUT | collect INPUT PLATFORM");
}
