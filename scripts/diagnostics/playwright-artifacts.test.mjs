import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

test("UI shards retain raw attachments on failure without duplicating successful blob evidence", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/ui-shards.yml", import.meta.url), "utf8");
  const steps = workflow.split(/^      - /m);
  const upload = steps.find(step => step.startsWith("name: Upload UI evidence\n"));
  assert.match(upload, /if: always\(\)/);
  assert.match(upload, /frontend\/blob-report\//);
  assert.match(upload, /frontend\/full-results.json/);
  assert.doesNotMatch(upload, /frontend\/test-results\//);
  const fallback = steps.find(step => step.startsWith("name: Preserve raw attachments after unsuccessful UI execution\n"));
  assert.match(fallback, /always\(\) && steps.ui.outcome != 'success'/);
  assert.match(fallback, /path: frontend\/test-results\//);
  assert.match(fallback, /name: ui-failure-attachments-/);
  assert.match(steps.find(step => step.startsWith("name: Run UI E2E\n")), /id: ui/);
});

test("real Playwright blobs restore passed and failed attachments after raw output is removed", { timeout: 60000 }, t => {
  const root = mkdtempSync(path.join(tmpdir(), "mesh-playwright-artifacts-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const cli = fileURLToPath(new URL("../../frontend/node_modules/playwright/cli.js", import.meta.url));
  const runner = fileURLToPath(new URL("../../frontend/node_modules/@playwright/test/index.js", import.meta.url));
  const config = path.join(root, "playwright.config.cjs");
  mkdirSync(path.join(root, "specs"));
  writeFileSync(config, "module.exports = { testDir: './specs', outputDir: './test-results', workers: 1, retries: 0 };\n");
  // Opaque binary and PNG bytes exercise attachment transport, not trace playback.
  const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aL1sAAAAASUVORK5CYII=", "base64");
  const binary = Buffer.from([0, 255, 17, 128, 10, 13, 0, 42]);
  writeFileSync(path.join(root, "specs/attachments.spec.js"), `
const { test, expect } = require(${JSON.stringify(runner)});
const { writeFileSync } = require('node:fs');
for (const fail of [false, true]) test(fail ? 'failed evidence' : 'passed evidence', async ({}, info) => {
  for (const [name, type, base64] of ${JSON.stringify([["screenshot.png", "image/png", png.toString("base64")], ["binary.dat", "application/octet-stream", binary.toString("base64")]])}) {
    const file = info.outputPath(name);
    writeFileSync(file, Buffer.from(base64, 'base64'));
    await info.attach(name, { path: file, contentType: type });
  }
  expect(fail).toBe(false);
});
`);
  const env = { ...process.env, PLAYWRIGHT_BLOB_OUTPUT_DIR: path.join(root, "blob-report"), PLAYWRIGHT_JSON_OUTPUT_NAME: path.join(root, "original.json"), PLAYWRIGHT_HTML_OPEN: "never", PLAYWRIGHT_HTML_OUTPUT_DIR: path.join(root, "html-report") };
  const run = (args, scoped = env) => spawnSync(process.execPath, [cli, ...args], { cwd: root, env: scoped, encoding: "utf8", timeout: 45000, maxBuffer: 8 * 1024 * 1024 });
  const execution = run(["test", "--config", config, "--reporter=blob,json"]);
  assert.equal(execution.status, 1, `${execution.error ?? ""}\n${execution.stdout}\n${execution.stderr}`);
  const original = JSON.parse(readFileSync(path.join(root, "original.json"), "utf8"));
  assert.equal(original.stats.expected, 1);
  assert.equal(original.stats.unexpected, 1);
  assert.equal(readdirSync(path.join(root, "blob-report")).filter(name => name.endsWith(".zip")).length, 1);
  rmSync(path.join(root, "test-results"), { recursive: true });
  const merged = run(["merge-reports", "--config", config, "--reporter=json,html", "blob-report"], { ...env, PLAYWRIGHT_JSON_OUTPUT_NAME: path.join(root, "merged.json") });
  assert.equal(merged.status, 0, `${merged.error ?? ""}\n${merged.stdout}\n${merged.stderr}`);
  const report = JSON.parse(readFileSync(path.join(root, "merged.json"), "utf8"));
  assert.equal(report.stats.expected, 1);
  assert.equal(report.stats.unexpected, 1);
  const results = report.suites.flatMap(suite => suite.specs.flatMap(spec => spec.tests.flatMap(test => test.results)));
  assert.equal(results.length, 2);
  for (const result of results) {
    for (const [name, bytes] of [["screenshot.png", png], ["binary.dat", binary]]) {
      const attachment = result.attachments.find(item => item.name === name);
      assert.ok(attachment?.path, `missing restored ${name}`);
      assert.deepEqual(readFileSync(path.resolve(root, attachment.path)), bytes);
    }
  }
  const digests = readdirSync(path.join(root, "html-report/data")).map(name => createHash("sha256").update(readFileSync(path.join(root, "html-report/data", name))).digest("hex"));
  for (const bytes of [png, binary]) assert.ok(digests.includes(createHash("sha256").update(bytes).digest("hex")), "HTML report lost an attachment");
});
