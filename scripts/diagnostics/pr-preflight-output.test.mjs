import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, realpathSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

test("separate browser stages isolate server ports, results and HTML reports", () => {
  const root = mkdtempSync(path.join(tmpdir(), "mesh-preflight-output-"));
  try {
    for (const directory of ["scripts/validation", "scripts/pr-platform", "frontend/node_modules/playwright"])
      mkdirSync(path.join(root, directory), { recursive: true });
    for (const filename of ["validation/browser.mjs", "pr-platform/guest.mjs", "pr-platform/plan.mjs", "pr-platform/rust-packages.mjs"])
      copyFileSync(new URL(`../${filename}`, import.meta.url), path.join(root, "scripts", filename));
    const trace = path.join(root, "trace.jsonl");
    writeFileSync(path.join(root, "frontend/node_modules/playwright/cli.js"), `
      const fs = require('node:fs');
      fs.writeFileSync(process.env.PLAYWRIGHT_JSON_OUTPUT_FILE, JSON.stringify({ stats: { expected: 1, unexpected: 0, flaky: 0 }, errors: [] }));
      fs.appendFileSync(process.env.PREFLIGHT_TRACE, JSON.stringify({ args: process.argv.slice(2), html: process.env.PLAYWRIGHT_HTML_OUTPUT_DIR, port: process.env.MESH_TALK_E2E_PORT, budget: process.env.MESH_BROWSER_TIMEOUT_SECONDS }) + '\\n');
    `);
    for (let run = 0; run < 2; run++) {
      const result = spawnSync(process.execPath, [path.join(root, "scripts/validation/browser.mjs"), "full", "e2e/settings-recovery.spec.ts"], {
        cwd: path.join(root, "frontend"), env: { ...process.env, PREFLIGHT_TRACE: trace }, encoding: "utf8", timeout: 15_000,
      });
      assert.equal(result.status, 0, result.stdout + result.stderr);
    }
    const commands = readFileSync(trace, "utf8").trim().split("\n").map(JSON.parse);
    assert.equal(commands.length, 2);
    const outputs = commands.map(({ args, html, port, budget }) => {
      const position = args.indexOf("--output");
      assert(position >= 0);
      const directory = args[position + 1];
      assert(directory.startsWith(path.join(realpathSync(root), "target") + path.sep));
      assert(args.includes("e2e/settings-recovery.spec.ts"));
      assert.equal(path.dirname(html), path.dirname(directory));
      assert.notEqual(html, directory);
      assert.equal(Number(budget), 7200, "a full suite must not inherit the 30-minute guest process limit");
      assert(Number(port) > 0 && Number(port) < 65536);
      assert(args.includes("--workers=1") && args.includes("--retries=0"));
      return directory;
    });
    assert.notEqual(outputs[0], outputs[1]);
    const controller = readFileSync(new URL("../pr-preflight.sh", import.meta.url), "utf8");
    assert(controller.includes('--resource heavy'));
    assert(controller.includes('scripts/validation/browser.mjs coverage'));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

// A process timeout must remain distinct from a UI assertion failure.
test("browser wrapper exposes timeout diagnostics and never writes success evidence", () => {
  const root = mkdtempSync(path.join(tmpdir(), "mesh-browser-timeout-"));
  try {
    for (const directory of ["scripts/validation", "scripts/pr-platform", "frontend/node_modules/playwright"])
      mkdirSync(path.join(root, directory), { recursive: true });
    for (const filename of ["validation/browser.mjs", "pr-platform/guest.mjs", "pr-platform/plan.mjs", "pr-platform/rust-packages.mjs"])
      copyFileSync(new URL(`../${filename}`, import.meta.url), path.join(root, "scripts", filename));
    writeFileSync(path.join(root, "frontend/node_modules/playwright/cli.js"), "setTimeout(() => {}, 3000);\n");
    const result = spawnSync(process.execPath, [path.join(root, "scripts/validation/browser.mjs"), "full"], {
      cwd: path.join(root, "frontend"), env: { ...process.env, MESH_BROWSER_TIMEOUT_SECONDS: "1" }, encoding: "utf8", timeout: 10_000,
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr + result.stdout, /execution budget.*1 second/i);
    assert.equal(existsSync(path.join(root, "target/validation/browser-full.json")), false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
