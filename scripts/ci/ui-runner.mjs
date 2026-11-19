import assert from "node:assert/strict";
import { balancedCasePartition, fullCaseInventory } from "../evals/chromium-evidence.mjs";
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { Writable } from "node:stream";
import { randomUUID } from "node:crypto";
import { execute as executeOwned } from "../validation/stage.mjs";

/** Command ownership and interrupted-run evidence; acceptance stays in existing collectors. */
export async function runUi(args, {
  deadline = Number(process.env.MESH_UI_DEADLINE_MS),
  directory = resolve(process.env.MESH_UI_EVIDENCE_DIR ?? "ui-runner-evidence"),
  execute = executeOwned,
  command = [process.execPath, resolve("node_modules/@playwright/test/cli.js"), "test", ...args],
  onOutput = chunk => process.stdout.write(chunk),
  preparation = null,
  cwd = process.cwd(),
} = {}) {
  if (!Number.isSafeInteger(deadline) || deadline <= Date.now()) throw Error("UI job deadline is missing or exhausted");
  await mkdir(directory, { recursive: true });
  const invocation = randomUUID();
  const started = Date.now();
  await writeFile(join(directory, "owner.json"), JSON.stringify({ state: "running", invocation, started, deadline, preparation, success: false }, null, 2));
  const controller = new AbortController();
  let timedOut = false;
  const abort = () => controller.abort();
  process.on("SIGTERM", abort);
  process.on("SIGINT", abort);
  const timer = setTimeout(() => { timedOut = true; abort(); }, Math.max(1, deadline - Date.now()));
  const shard = args.find(arg => arg.startsWith("--shard="))?.slice(8) ?? null;
  const env = { ...process.env, ...(preparation ? { DEBUG: [process.env.DEBUG, "pw:install"].filter(Boolean).join(",") } : {}), MESH_UI_INVOCATION_ID: invocation, MESH_UI_EVIDENCE_DIR: directory, MESH_UI_SHARD: shard ?? "" };
  let outputBytes = 0;
  const forwardOutput = preparation ? chunk => {
    const captured = chunk.subarray(0, Math.max(0, preparation.logLimitBytes - outputBytes));
    outputBytes += captured.length;
    if (captured.length) onOutput(captured);
  } : onOutput;
  let outcome;
  try {
    if (!preparation && process.env.MESH_UI_BALANCED_PARTITION === "1") {
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
    outcome = await execute(command, { cwd, env,
      ...(preparation ? { createLogStream: filename => boundedPreparationLog(filename) } : {}),
      terminationSignal: "SIGINT", logPath: join(directory, "execution.log"), signal: controller.signal, onOutput: forwardOutput });
  } catch (error) {
    outcome = { code: 1, error: error.message };
  } finally {
    clearTimeout(timer);
    process.off("SIGTERM", abort);
    process.off("SIGINT", abort);
  }
  const sourceSha = process.env.EVAL_SOURCE_SHA ?? null;
  if (!preparation && process.platform !== "win32" && (controller.signal.aborted || outcome.code !== 0 || outcome.error || outcome.signal || outcome.forcedRetirement)) {
    let proof;
    try { proof = JSON.parse(await readFile(join(directory, `${invocation}-teardown.json`), "utf8")); } catch {}
    if (proof?.invocation !== invocation || proof.sourceSha !== sourceSha || proof.shard !== shard || proof.errors !== 0) {
      outcome.cleanupUnverified = true;
      outcome.error ||= "Playwright cooperative teardown was not verified; detached server cleanup is unknown";
    }
  }
  const result = { ...outcome, invocation, state: "finished", started, deadline, elapsedMs: Date.now() - started, timedOut,
    sourceSha, shard, preparation,
    success: outcome.code === 0 && !controller.signal.aborted && !outcome.error && !outcome.cleanupUnverified };
  await writeFile(join(directory, "owner.json"), JSON.stringify(result, null, 2));
  return result;
}

// Installation has no independently grouped web server: the existing process
// owner verifies its installer/download descendants, including bounded draining.
function boundedPreparationLog(filename) {
  const limit = 8 * 1024 * 1024;
  let bytes = 0;
  writeFileSync(filename, "");
  return new Writable({ write(chunk, _encoding, done) {
    const remaining = limit - bytes;
    const captured = chunk.subarray(0, remaining);
    try { appendFileSync(filename, captured); } catch (error) { done(error); return; }
    bytes += captured.length;
    done(chunk.length > remaining ? Error("Browser preparation log exceeded 8 MiB") : null);
  } });
}

export async function prepareBrowser(browser, {
  deadline = Number(process.env.MESH_UI_DEADLINE_MS),
  frontend = process.cwd(),
  directory = resolve(process.env.MESH_UI_EVIDENCE_DIR ?? "ui-runner-evidence", "browser-preparation"),
  ...options
} = {}) {
  assert.equal(browser, "webkit", "Unsupported preparation browser");
  assert.ok(Number.isSafeInteger(deadline) && deadline > Date.now(), "UI job deadline is missing or exhausted");
  const packageRoot = resolve(frontend, "node_modules/playwright-core");
  const manifest = JSON.parse(await readFile(join(packageRoot, "package.json"), "utf8"));
  const browsers = JSON.parse(await readFile(join(packageRoot, "browsers.json"), "utf8"));
  const descriptor = browsers.browsers.find(value => value.name === browser);
  assert.ok(descriptor?.revision, "Pinned browser revision is missing");
  return runUi([], { ...options, directory, cwd: frontend,
    deadline: Math.min(deadline, Date.now() + 5 * 60000),
    command: [process.execPath, resolve(frontend, "node_modules/@playwright/test/cli.js"), "install", "--with-deps", browser],
    preparation: { browser, playwrightVersion: manifest.version, browserRevision: descriptor.revision,
      revisionOverrides: descriptor.revisionOverrides ?? {}, logLimitBytes: 8 * 1024 * 1024, maximumMs: 5 * 60000 },
  });
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
    const args = process.argv.slice(2);
    const result = args.length === 1 && args[0].startsWith("--install-browser=")
      ? await prepareBrowser(args[0].slice("--install-browser=".length))
      : await runUi(args);
    console.log(JSON.stringify(result));
    process.exitCode = result.success ? 0 : 1;
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
