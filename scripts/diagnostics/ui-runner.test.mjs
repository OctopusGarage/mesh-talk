import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runUi } from "../ci/ui-runner.mjs";
import { spawn } from "node:child_process";

test("expired job budget never starts the browser", async () => {
  let started = false;
  await assert.rejects(runUi([], { deadline: 1, execute: async () => { started = true; } }), /deadline/);
  assert.equal(started, false);
});

test("owned absolute deadline rejects unfinished work without a startup SLA", async () => {
  const directory = await mkdtemp(join(tmpdir(), "mesh-ui-owner-"));
  const previousOwner = process.env.MESH_VALIDATION_PROCESS_GROUP_OWNER;
  delete process.env.MESH_VALIDATION_PROCESS_GROUP_OWNER;
  try {
    const result = await runUi([], {
      deadline: Date.now() + 600,
      directory,
      command: [process.execPath, "-e", "console.log('browser-started'); setInterval(()=>{},1000)"],
      onOutput() {},
    });
    assert.equal(result.timedOut, true);
    assert.equal(result.success, false);
    assert.equal(result.cleanupUnverified, false);
    assert.equal(JSON.parse(await readFile(join(directory, "owner.json"), "utf8")).timedOut, true);
  } finally {
    if (previousOwner !== undefined) process.env.MESH_VALIDATION_PROCESS_GROUP_OWNER = previousOwner;
    await rm(directory, { recursive: true, force: true });
  }
});

test("verified retirement preserves the canonical exit verdict while cleanup uncertainty fails", async () => {
  const directory = await mkdtemp(join(tmpdir(), "mesh-ui-result-"));
  try {
    for (const [outcome, expected] of [
      [{ code: 0, cleanupUnverified: true }, false],
      [{ code: 0, forcedRetirement: true, cleanupUnverified: false }, true],
      [{ code: 1 }, false],
      [{ code: 0, error: "owner failure" }, false],
      [{ code: 0 }, true],
    ]) {
      const result = await runUi([], { deadline: Date.now() + 10000, directory, execute: async () => outcome });
      assert.equal(result.success, expected);
      assert.equal(result.forcedRetirement, outcome.forcedRetirement);
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("real Playwright interruption retains active-case evidence and fails the owned command", { timeout: 30000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "mesh-ui-playwright-"));
  const { writeFile } = await import("node:fs/promises");
  const { fileURLToPath } = await import("node:url");
  const reporter = fileURLToPath(new URL("../ci/ui-runner.mjs", import.meta.url));
  const runner = fileURLToPath(new URL("../../frontend/node_modules/@playwright/test/index.js", import.meta.url));
  const previous = process.env.MESH_UI_EVIDENCE_DIR;
  process.env.MESH_UI_EVIDENCE_DIR = directory;
  try {
    await writeFile(join(directory, "playwright.config.cjs"), `module.exports={testDir:__dirname,testMatch:'parked.spec.cjs',timeout:0,workers:1,retries:0,outputDir:${JSON.stringify(join(directory, 'results'))}};`);
    await writeFile(join(directory, "parked.spec.cjs"), `const {test}=require(${JSON.stringify(runner)}); test('parked command',async()=>{console.log('PARKED_READY');await new Promise(()=>{});});`);
    const env = { ...process.env, MESH_UI_DEADLINE_MS: String(Date.now() + 60000) };
    // This is an independent CI entrypoint, not a nested validation stage.
    delete env.MESH_VALIDATION_PROCESS_GROUP_OWNER;
    // Deliver the signal event inside Node: Windows kill(SIGTERM) bypasses
    // JavaScript handlers. This preload belongs only to the test process.
    const child = spawn(process.execPath, ["--import", "data:text/javascript,if(!process.env.MESH_UI_TEST_SIGNAL_INSTALLED){process.env.MESH_UI_TEST_SIGNAL_INSTALLED='1';const write=process.stdout.write.bind(process.stdout);process.stdout.write=(chunk,...args)=>{const result=write(chunk,...args);if(String(chunk).includes('PARKED_READY'))queueMicrotask(()=>process.emit('SIGTERM'));return result}}", reporter, "--config", join(directory, "playwright.config.cjs"), "--reporter", `list,${reporter}`], {
      cwd: fileURLToPath(new URL("../../frontend/", import.meta.url)), env,
      signal: AbortSignal.timeout(25000), stdio: ["ignore", "pipe", "pipe"],
    });
    let output = "";
    let interrupted = false;
    child.stdout.on("data", chunk => {
      output += chunk;
      if (!interrupted && output.includes("PARKED_READY")) { interrupted = true; }
    });
    child.stderr.on("data", chunk => { output += chunk; });
    const code = await new Promise((resolve, reject) => { child.once("error", reject); child.once("close", resolve); });
    assert.equal(interrupted, true, output);
    assert.equal(code, 1, "the actual CI entrypoint must exit nonzero");
    const owner = JSON.parse(await readFile(join(directory, "owner.json"), "utf8"));
    assert.equal(owner.success, false);
    assert.equal(owner.cleanupUnverified, false);
    assert.match(await readFile(join(directory, "execution.log"), "utf8"), /PARKED_READY/);
    const events = (await readFile(join(directory, "progress.jsonl"), "utf8")).trim().split("\n").map(JSON.parse);
    assert(events.some(event => event.event === "active" && event.title === "parked command"));
  } finally {
    if (previous === undefined) delete process.env.MESH_UI_EVIDENCE_DIR;
    else process.env.MESH_UI_EVIDENCE_DIR = previous;
    await rm(directory, { recursive: true, force: true });
  }
});
