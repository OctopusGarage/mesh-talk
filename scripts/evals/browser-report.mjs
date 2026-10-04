import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFile, realpath, writeFile } from "node:fs/promises";
import { dirname, resolve, relative, isAbsolute } from "node:path";
import { pathToFileURL } from "node:url";
import { createHash } from "node:crypto";
import { inspectPng } from "./png-evidence.mjs";
import { inspectTrace } from "./trace-evidence.mjs";

const EVIDENCE = ["portable-browser-evidence", "portable-browser-screenshot", "trace"];
export function requiredBrowserPaths(tier) {
  const paths = [
    ["portable-core.spec.ts", "portable authentication rejects the wrong password"],
    ["portable-core.spec.ts", "portable registration sign in and sign out"],
    ["portable-core.spec.ts", "light theme persists across browser reload and sign in"],
  ];
  for (const size of ["760x520", "1040x720"]) for (const title of [
    "shell and avatar expose bounded interactive targets", "DM sends and renders incoming event delivery",
    "group conversation sends and renders incoming events", "profile dialog fits and preserves keyboard focus while renaming",
    "settings switches light dark and Chinese English within bounds", "Unicode multiline and unbroken wide messages stay in the log",
  ]) paths.push(["portable-core.spec.ts", `${size} browser-only core`, title]);
  if (tier === "extended") paths.push(["portable-core.spec.ts", "@extended palette locale resize and repeated dialog DOM retention smoke"]);
  return paths;
}
export function validateBrowserReport(report, { count, browser, expectedPaths }) {
  assert.equal(report.stats?.expected, count, "Missing or zero executed portable scenarios");
  for (const key of ["unexpected", "skipped", "flaky"]) assert.equal(report.stats?.[key], 0, `Nonpassing portable result: ${key}`);
  assert.deepEqual(report.errors, [], "Portable browser harness errors");
  const results = [];
  const seenIds = new Set(), seenPaths = new Set(), seenFiles = new Set();
  function visit(suite, parents = []) {
    const suitePath = [...parents, suite.title];
    for (const spec of suite.specs ?? []) {
      assert.equal(spec.ok, true, "Failed portable spec");
      const scenarioPath = [...suitePath, spec.title], key = JSON.stringify(scenarioPath);
      assert.ok(typeof spec.id === "string" && !seenIds.has(spec.id) && !seenPaths.has(key), "Duplicate or unidentified portable scenario");
      seenIds.add(spec.id); seenPaths.add(key);
      for (const test of spec.tests ?? []) {
        assert.equal(test.projectName, browser, "Unexpected browser evidence");
        assert.equal(test.status, "expected", "Nonpassing scenario outcome");
        assert.equal(test.results.length, 1, "Retries cannot conceal flaky portable scenarios");
        const result = test.results[0];
        assert.equal(result.status, "passed", "Missing actual scenario pass");
        for (const name of EVIDENCE) {
          const files = result.attachments.filter(attachment => attachment.name === name && typeof attachment.path === "string");
          assert.equal(files.length, 1, `Missing unique evidence: ${name}`);
          const path = resolve(files[0].path);
          assert.ok(!seenFiles.has(path), "Evidence file reused across scenarios");
          seenFiles.add(path);
        }
        results.push({ ...result, scenarioPath, scenarioTitle: spec.title, testId: spec.id });
      }
    }
    for (const child of suite.suites ?? []) visit(child, suitePath);
  }
  for (const suite of report.suites ?? []) visit(suite);
  assert.equal(results.length, count, "Portable report statistics do not match executed cases");
  if (expectedPaths) assert.deepEqual([...seenPaths].sort(), expectedPaths.map(path => JSON.stringify(path)).sort(), "Expected portable workflow matrix differs from executed scenarios");
  return results;
}

export function validateScenarioIdentity(evidence, result) {
  assert.deepEqual(evidence.scenarioPath, result.scenarioPath, "Evidence belongs to a different portable scenario");
  assert.equal(evidence.scenario, result.scenarioTitle, "Scenario title substitution");
}

async function main() {
  const reportPath = resolve(process.argv[2] ?? "frontend/eval-results/portable/chromium/core/results.json");
  const root = await realpath(dirname(reportPath));
  const tier = process.env.EVAL_TIER ?? "core", browser = process.env.EVAL_BROWSER ?? "chromium";
  assert.ok(["core", "extended"].includes(tier), "Unsupported portable tier");
  assert.ok(["chromium", "webkit", "firefox"].includes(browser), "Unsupported portable browser");
  const sourceSha = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
  assert.match(sourceSha, /^[a-f0-9]{40}$/);
  if (process.env.EVAL_SOURCE_SHA !== undefined) assert.equal(process.env.EVAL_SOURCE_SHA, sourceSha, "Source revision mismatch");
  const report = JSON.parse(await readFile(reportPath, "utf8"));
  const expectedPaths = requiredBrowserPaths(tier);
  const results = validateBrowserReport(report, { count: expectedPaths.length, browser, expectedPaths });
  const scenarios = [];
  const canonicalFiles = new Set();
  for (const result of results) {
    const files = {};
    const digests = {};
    for (const name of EVIDENCE) {
      const path = await realpath(result.attachments.find(attachment => attachment.name === name).path);
      assert.ok(!canonicalFiles.has(path), "Evidence alias reused across scenarios");
      canonicalFiles.add(path);
      const local = relative(root, path);
      assert.ok(local !== ".." && !local.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`) && !isAbsolute(local), "Evidence must remain inside portable artifact directory");
      files[name] = await readFile(path);
      digests[name] = createHash("sha256").update(files[name]).digest("hex");
    }
    const evidence = JSON.parse(files["portable-browser-evidence"].toString("utf8"));
    validateScenarioIdentity(evidence, result);
    assert.equal(evidence.sourceSha, sourceSha, "Stale browser evidence");
    assert.equal(evidence.platform, process.platform, "Wrong operating system evidence");
    assert.equal(evidence.browser, browser, "Wrong browser evidence");
    assert.equal(evidence.mocked, true, "Browser IPC boundary must be disclosed");
    assert.equal(evidence.native, false, "Browser evidence cannot claim native coverage");
    assert.equal(evidence.status, "passed", "Scenario evidence reports failure");
    assert.deepEqual(evidence.pageErrors, [], "Uncaught page errors");
    assert.ok(!evidence.screenshotError && !evidence.fixtureError, "Failed evidence capture");
    const png = inspectPng(files["portable-browser-screenshot"]);
    assert.equal(png.width, evidence.viewport?.width, "Screenshot differs from tested viewport width");
    assert.equal(png.height, evidence.viewport?.height, "Screenshot differs from tested viewport height");
    const trace = inspectTrace(files.trace);
    scenarios.push({ scenario: evidence.scenario, scenarioPath: evidence.scenarioPath, passed: true, evidenceDigests: digests, screenshot: png, trace });
  }
  await writeFile(resolve(root, "validated-report.json"), JSON.stringify({ schema: 1, sourceSha, platform: process.platform, browser, tier, native: false, mocked: true, scenarios }, null, 2));
  console.log(`Validated ${scenarios.length} ${browser} ${process.platform} portable scenarios and their evidence`);
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main();
