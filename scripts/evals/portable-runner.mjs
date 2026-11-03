import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { requiredBrowserPaths, validateBrowserReport } from "./browser-report.mjs";

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
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await runPortable();
