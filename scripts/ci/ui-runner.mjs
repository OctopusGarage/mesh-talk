import assert from "node:assert/strict";
import { balancedCasePartition, fullCaseInventory } from "../evals/chromium-evidence.mjs";
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { randomUUID } from "node:crypto";
import { execute as executeOwned } from "../validation/stage.mjs";

/** Command ownership and interrupted-run evidence; acceptance stays in existing collectors. */
export async function runUi(args, {
  deadline = Number(process.env.MESH_UI_DEADLINE_MS),
  directory = resolve(process.env.MESH_UI_EVIDENCE_DIR ?? "ui-runner-evidence"),
  execute = executeOwned,
  command = [process.execPath, resolve("node_modules/@playwright/test/cli.js"), "test", ...args],
  onOutput = chunk => process.stdout.write(chunk),
} = {}) {
  if (!Number.isSafeInteger(deadline) || deadline <= Date.now()) throw Error("UI job deadline is missing or exhausted");
  await mkdir(directory, { recursive: true });
  const invocation = randomUUID();
  const started = Date.now();
  await writeFile(join(directory, "owner.json"), JSON.stringify({ state: "running", invocation, started, deadline, success: false }, null, 2));
  const controller = new AbortController();
  let timedOut = false;
  const abort = () => controller.abort();
  process.on("SIGTERM", abort);
  process.on("SIGINT", abort);
  const timer = setTimeout(() => { timedOut = true; abort(); }, Math.max(1, deadline - Date.now()));
  const shard = args.find(arg => arg.startsWith("--shard="))?.slice(8) ?? null;
  const env = { ...process.env, MESH_UI_INVOCATION_ID: invocation, MESH_UI_EVIDENCE_DIR: directory, MESH_UI_SHARD: shard ?? "" };
  let outcome;
  try {
    if (process.env.MESH_UI_BALANCED_PARTITION === "1") {
      assert.ok(shard, "Balanced execution requires a shard identity");
      const base = [process.execPath, resolve("node_modules/@playwright/test/cli.js"), "test"];
      const listing = join(directory, "listing.json");
      const selectedListing = join(directory, "selected-listing.json");
      const listPath = join(directory, "selected-tests.txt");
      const common = args.filter(arg => !arg.startsWith("--shard=") && !arg.startsWith("--reporter="));
      const list = async (path, extra = []) => {
        await rm(path, { force: true });
        const result = await execute([...base, ...common, ...extra, "--list", "--reporter=json"], {
          cwd: process.cwd(), env: { ...env, PLAYWRIGHT_JSON_OUTPUT_FILE: path },
          terminationSignal: "SIGINT", signal: controller.signal, logPath: `${path}.log`, onOutput });
        assert.ok(result.code === 0 && !result.error && !result.cleanupUnverified && !controller.signal.aborted, "Playwright inventory discovery failed");
        return JSON.parse(await readFile(path, "utf8"));
      };
      const selected = balancedCasePartition(await list(listing), shard);
      await writeFile(listPath, selected.map(value => value.line).join("\n") + "\n");
      assert.deepEqual(fullCaseInventory(await list(selectedListing, [`--test-list=${listPath}`])), selected, "Selected CLI inventory differs from partition");
      await writeFile(join(directory, "partition.json"), JSON.stringify({ sourceSha: env.EVAL_SOURCE_SHA, shard, cases: selected }));
      command = [...base, ...args.filter(arg => !arg.startsWith("--shard=")), `--test-list=${listPath}`];
    }
    outcome = await execute(command, { cwd: process.cwd(), env,
      terminationSignal: "SIGINT", logPath: join(directory, "execution.log"), signal: controller.signal, onOutput });
  } catch (error) {
    outcome = { code: 1, error: error.message };
  } finally {
    clearTimeout(timer);
    process.off("SIGTERM", abort);
    process.off("SIGINT", abort);
  }
  const sourceSha = process.env.EVAL_SOURCE_SHA ?? null;
  if (process.platform !== "win32" && (controller.signal.aborted || outcome.code !== 0 || outcome.error || outcome.signal || outcome.forcedRetirement)) {
    let proof;
    try { proof = JSON.parse(await readFile(join(directory, `${invocation}-teardown.json`), "utf8")); } catch {}
    if (proof?.invocation !== invocation || proof.sourceSha !== sourceSha || proof.shard !== shard || proof.errors !== 0) {
      outcome.cleanupUnverified = true;
      outcome.error ||= "Playwright cooperative teardown was not verified; detached server cleanup is unknown";
    }
  }
  const result = { ...outcome, invocation, state: "finished", started, deadline, elapsedMs: Date.now() - started, timedOut,
    sourceSha, shard,
    success: outcome.code === 0 && !controller.signal.aborted && !outcome.error && !outcome.cleanupUnverified };
  await writeFile(join(directory, "owner.json"), JSON.stringify(result, null, 2));
  return result;
}

/** Append-only bounded progress survives a killed runner without a finalized report. */
export default class UiProgressReporter {
  constructor() {
    this.directory = resolve(process.env.MESH_UI_EVIDENCE_DIR ?? "ui-runner-evidence");
    this.records = 0;
    this.harnessErrors = 0;
    this.identity = { invocation: process.env.MESH_UI_INVOCATION_ID ?? null, sourceSha: process.env.EVAL_SOURCE_SHA ?? null,
      shard: process.env.MESH_UI_SHARD || (process.argv.find(arg => arg.startsWith("--shard="))?.slice(8) ?? null) };
  }
  record(event, data = {}) {
    // Each bounded record represents lifecycle evidence, never acceptance.
    if (this.records++ >= 100000) throw Error("UI progress evidence exceeded record budget");
    mkdirSync(this.directory, { recursive: true });
    appendFileSync(join(this.directory, "progress.jsonl"), JSON.stringify({ ...this.identity,
      time: Date.now(), event, ...data }) + "\n");
  }
  onBegin(_config, suite) { this.record("begin", { count: suite.allTests().length }); }
  onTestBegin(test, result) { this.record("active", { id: test.id, title: test.title.slice(0, 2048), retry: result.retry }); }
  onTestEnd(test, result) { this.record("completed", { id: test.id, status: result.status, durationMs: result.duration }); }
  onEnd(result) { this.record("end", { status: result.status }); }
  onError() { this.harnessErrors++; }
  onExit() {
    this.record("exit", { errors: this.harnessErrors });
    // Playwright awaits plugin teardown before reporter.onExit. This proves
    // cooperative cleanup completed, beyond the outer POSIX process group.
    if (this.identity.invocation) writeFileSync(join(this.directory, `${this.identity.invocation}-teardown.json`), JSON.stringify({ ...this.identity, errors: this.harnessErrors }));
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const result = await runUi(process.argv.slice(2));
    console.log(JSON.stringify(result));
    process.exitCode = result.success ? 0 : 1;
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
