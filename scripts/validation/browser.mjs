#!/usr/bin/env node
/** Own the server port and coverage output for one serialized browser stage. */
import { createServer } from "node:net";
import { mkdir, readFile, readdir, unlink, writeFile } from "node:fs/promises";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { runCommand } from "../pr-platform/guest.mjs";

const [mode, ...specs] = process.argv.slice(2);
if (!["coverage", "full", "portable"].includes(mode)) throw Error("Expected coverage, full, or portable browser mode");
const timeoutSeconds = Number(process.env.MESH_BROWSER_TIMEOUT_SECONDS ?? (mode === "full" ? 7200 : 1800));
if (!Number.isInteger(timeoutSeconds) || timeoutSeconds < 1 || timeoutSeconds > 7200)
  throw Error("MESH_BROWSER_TIMEOUT_SECONDS must be an integer from 1 to 7200");
const root = path.resolve("..");
const directory = path.join(root, "target/validation/browser", `${Date.now()}-${randomUUID()}`);
await mkdir(directory, { recursive: true });
const server = createServer();
await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
const port = server.address().port;
await new Promise(resolve => server.close(resolve));
const env = { ...process.env, MESH_BROWSER_TIMEOUT_SECONDS: String(timeoutSeconds), MESH_TALK_E2E_PORT: String(port), EVAL_UI_PORT: String(port), PLAYWRIGHT_HTML_OUTPUT_DIR: path.join(directory, "report"), PLAYWRIGHT_JSON_OUTPUT_FILE: path.join(directory, "results.json") };
if (mode === "portable") {
  env.EVAL_EVIDENCE_ROOT = path.join(directory, "portable");
  delete env.PLAYWRIGHT_JSON_OUTPUT_FILE;
}
async function checked(args) {
  const result = await runCommand(process.execPath, args, { cwd: process.cwd(), env, timeoutMs: timeoutSeconds * 1000, logPath: path.join(directory, `step-${checked.index++}.log`) });
  process.stdout.write(result.output);
  if (result.timedOut) throw Error(`Browser ${mode} execution budget exceeded (${timeoutSeconds} seconds); evidence: ${directory}`);
  if (result.code !== 0) process.exit(result.code || 1);
}
checked.index = 0;
if (mode === "coverage") {
  const parts = path.resolve("coverage/e2e-parts");
  await mkdir(parts, { recursive: true });
  for (const name of await readdir(parts)) if (name.endsWith(".json")) await unlink(path.join(parts, name));
  env.MESH_TALK_E2E_COVERAGE = "1";
}
const cli = path.resolve("node_modules/playwright/cli.js");
await checked(mode === "portable"
  ? [cli, "test", "--config", "playwright.portable.config.ts"]
  : [cli, "test", "--project=chromium", "--workers=1", "--retries=0", "--reporter=list,json", "--output", path.join(directory, "results"), ...specs]);
if (mode === "portable") {
  await checked(["../scripts/evals/browser-report.mjs", path.join(env.EVAL_EVIDENCE_ROOT, "results.json")]);
} else {
  const report = JSON.parse(await readFile(env.PLAYWRIGHT_JSON_OUTPUT_FILE, "utf8"));
  if (!(report.stats?.expected > 0) || report.stats.unexpected !== 0 || report.stats.flaky !== 0 || report.errors?.length !== 0) throw Error("Browser run did not produce complete passing evidence");
}
if (mode === "coverage") await checked(["scripts/merge-e2e-coverage.mjs"]);
await writeFile(path.join(root, `target/validation/browser-${mode}.json`), JSON.stringify([directory], null, 2));
console.log(`Browser ${mode} evidence: ${directory}`);
