import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
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
    assert.equal(result.cleanupUnverified, process.platform !== "win32");
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
      const result = await runUi([], { deadline: Date.now() + 10000, directory, execute: async (_command, { env }) => {
        if (outcome.forcedRetirement && !outcome.cleanupUnverified) await writeFile(join(directory, `${env.MESH_UI_INVOCATION_ID}-teardown.json`), JSON.stringify({ invocation: env.MESH_UI_INVOCATION_ID, sourceSha: process.env.EVAL_SOURCE_SHA ?? null, shard: null, errors: 0 }));
        return outcome;
      } });
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
  let serverPid;
  try {
    const server = join(directory, "server.cjs");
    const marker = join(directory, "server-pid");
    await writeFile(server, `require('node:fs').writeFileSync(${JSON.stringify(marker)},String(process.pid));console.log('SERVER_READY');setInterval(()=>{},1000);`);
    const command = `"${process.execPath}" "${server}"`;
    await writeFile(join(directory, "playwright.config.cjs"), `module.exports={testDir:__dirname,testMatch:'parked.spec.cjs',timeout:0,workers:1,retries:0,webServer:{command:${JSON.stringify(command)},wait:{stdout:/SERVER_READY/}},outputDir:${JSON.stringify(join(directory, 'results'))}};`);
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
    serverPid = Number(await readFile(marker, "utf8"));
    assert.equal(interrupted, true, output);
    assert.equal(code, 1, "the actual CI entrypoint must exit nonzero");
    const owner = JSON.parse(await readFile(join(directory, "owner.json"), "utf8"));
    assert.equal(owner.success, false);
    assert.throws(() => process.kill(serverPid, 0), { code: "ESRCH" }, "the separately launched webServer must retire");
    assert.equal(owner.cleanupUnverified, false, owner.error);
    assert.match(await readFile(join(directory, "execution.log"), "utf8"), /PARKED_READY/);
    const events = (await readFile(join(directory, "progress.jsonl"), "utf8")).trim().split("\n").map(JSON.parse);
    assert(events.some(event => event.event === "active" && event.title === "parked command"));
  } finally {
    if (serverPid) { try { process.kill(serverPid, "SIGTERM"); } catch (error) { if (error.code !== "ESRCH") throw error; } }
    if (previous === undefined) delete process.env.MESH_UI_EVIDENCE_DIR;
    else process.env.MESH_UI_EVIDENCE_DIR = previous;
    await rm(directory, { recursive: true, force: true });
  }
});

test("a killed POSIX browser cannot borrow stale or mismatched cooperative cleanup proof", { skip: process.platform === "win32" }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "mesh-ui-teardown-proof-"));
  const { writeFile } = await import("node:fs/promises");
  try {
    for (const [change, expected] of [[null, true], [{ invocation: "old" }, true], [{ sourceSha: "stale" }, true], [{ shard: "2/2" }, true], [{ errors: 1 }, true], [{}, false]]) {
      const result = await runUi(["--shard=1/2"], { deadline: Date.now() + 10000, directory,
        execute: async (_command, { env }) => {
          if (change !== null) await writeFile(join(directory, `${env.MESH_UI_INVOCATION_ID}-teardown.json`), JSON.stringify({ invocation: env.MESH_UI_INVOCATION_ID, sourceSha: process.env.EVAL_SOURCE_SHA ?? null, shard: "1/2", errors: 0, ...change }));
          return { code: 1, signal: "SIGKILL", cleanupUnverified: false };
        },
      });
      assert.equal(result.cleanupUnverified, expected);
      assert.equal(result.success, false, "cleanup evidence must never certify test acceptance");
    }
    for (const outcome of [{ code: 1, signal: "SIGTERM" }, { code: 1 }, { code: 0, error: "output failure" }, { code: 0, forcedRetirement: true }]) {
      const result = await runUi([], { deadline: Date.now() + 10000, directory, execute: async () => ({ cleanupUnverified: false, ...outcome }) });
      assert.equal(result.cleanupUnverified, true, "abnormal completion without cooperative proof leaves detached work unknown");
      assert.equal(result.success, false);
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("balanced discovery fails closed and preserves original shard identity without double filtering", async () => {
  const directory = await mkdtemp(join(tmpdir(), "mesh-ui-balanced-"));
  const previous = process.env.MESH_UI_BALANCED_PARTITION;
  process.env.MESH_UI_BALANCED_PARTITION = "1";
  const fixture = { suites: [{ specs: ["a", "b"].map(id => ({ id, file: "fixture.spec.ts", title: id, tests: [{ projectName: "chromium" }] })) }] };
  try {
    for (const failure of ["discovery", "selection", null]) {
      const commands = [];
      const result = await runUi(["--workers=1", "--retries=0", "--shard=1/2", "--reporter=list"], { directory, deadline: Date.now() + 10000, execute: async (command, { env }) => {
        commands.push(command);
        assert.equal(env.MESH_UI_SHARD, "1/2");
        if (command.includes("--list")) {
          assert.ok(!command.some(value => value.startsWith("--shard=")));
          if (failure === "discovery") return { code: 1 };
          const selected = command.some(value => value.startsWith("--test-list="));
          const report = structuredClone(fixture);
          if (selected && failure !== "selection") report.suites[0].specs.pop();
          await writeFile(env.PLAYWRIGHT_JSON_OUTPUT_FILE, JSON.stringify(report));
          return { code: 0 };
        }
        assert.ok(command.some(value => value.startsWith("--test-list=")));
        assert.ok(!command.some(value => value.startsWith("--shard=")));
        assert.ok(command.includes("--workers=1") && command.includes("--retries=0") && command.includes("--reporter=list"));
        return { code: 0 };
      } });
      assert.equal(result.success, failure === null);
      assert.equal(result.shard, "1/2");
      assert.equal(commands.length, failure === "discovery" ? 1 : failure === "selection" ? 2 : 3);
    }
  } finally {
    if (previous === undefined) delete process.env.MESH_UI_BALANCED_PARTITION; else process.env.MESH_UI_BALANCED_PARTITION = previous;
    await rm(directory, { recursive: true, force: true });
  }
});

test("browser preparation retains the pinned installer and a separate bounded receipt", async () => {
  const { prepareBrowser } = await import("../ci/ui-runner.mjs");
  assert.equal(typeof prepareBrowser, "function");
  const directory = await mkdtemp(join(tmpdir(), "mesh-browser-prepare-"));
  const { fileURLToPath } = await import("node:url");
  const frontend = fileURLToPath(new URL("../../frontend/", import.meta.url));
  try {
    let invoked = 0;
    const result = await prepareBrowser("webkit", { directory, frontend, deadline: Date.now() + 600000, execute: async (command, options) => {
      invoked++;
      assert.equal(command[0], process.execPath);
      assert.equal(command[1], join(frontend, "node_modules/@playwright/test/cli.js"));
      assert.deepEqual(command.slice(2), ["install", "--with-deps", "webkit"]);
      assert.match(options.env.DEBUG, /pw:install/);
      return { code: 0 };
    } });
    assert.equal(result.success, true);
    assert.equal(invoked, 1, "installer already owns transport retries");
    assert.equal(result.deadline - result.started <= 300000, true);
    assert.equal(result.preparation.browser, "webkit");
    assert.ok(result.preparation.playwrightVersion && result.preparation.browserRevision);
    assert.deepEqual(JSON.parse(await readFile(join(directory, "owner.json"))), result);
    await assert.rejects(prepareBrowser("unknown", { directory, frontend }), /browser/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("real preparation timeout and cancellation retire admitted child work and preserve diagnostics", { timeout: 20000 }, async () => {
  const { prepareBrowser } = await import("../ci/ui-runner.mjs");
  const { execute } = await import("../validation/stage.mjs");
  const { fileURLToPath } = await import("node:url");
  const frontend = fileURLToPath(new URL("../../frontend/", import.meta.url));
  // This fixture owns an independent installer command, even inside Health.
  // Nested commands otherwise await retirement by the still-live outer owner.
  for (const mode of ["timeout", "cancel"]) {
    const directory = await mkdtemp(join(tmpdir(), "mesh-browser-retire-"));
    try {
      const result = await prepareBrowser("webkit", { directory, frontend, deadline: Date.now() + (mode === "timeout" ? 800 : 10000),
        onOutput(chunk) { if (mode === "cancel" && String(chunk).includes("PREPARATION_READY")) process.emit("SIGTERM"); },
        execute: (_command, options) => execute([process.execPath, "-e", "console.error('first installer diagnostic');console.log('PREPARATION_READY');setInterval(()=>{},1000)"], { ...options, env: { ...options.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" } }),
      });
      assert.equal(result.success, false);
      assert.equal(result.timedOut, mode === "timeout");
      assert.ok(!result.cleanupUnverified, JSON.stringify(result));
      const log = await readFile(join(directory, "execution.log"), "utf8");
      // Timeout need not admit a child before its deadline. Cancellation is
      // triggered by the child readiness marker, so that cohort owns this proof.
      if (mode === "cancel") assert.match(log, /first installer diagnostic/);
      assert.equal(JSON.parse(await readFile(join(directory, "owner.json"))).success, false);
    } finally { await rm(directory, { recursive: true, force: true }); }
  }
});

test("preparation output has a hard retained byte limit and uncertainty cannot certify success", { timeout: 15000 }, async () => {
  const { prepareBrowser } = await import("../ci/ui-runner.mjs");
  const { execute } = await import("../validation/stage.mjs");
  const { fileURLToPath } = await import("node:url");
  const directory = await mkdtemp(join(tmpdir(), "mesh-browser-output-"));
  const frontend = fileURLToPath(new URL("../../frontend/", import.meta.url));
  try {
    const result = await prepareBrowser("webkit", { directory, frontend, deadline: Date.now() + 12000, onOutput() {},
      execute: (_command, options) => execute([process.execPath, "-e", "process.stdout.write(Buffer.alloc(9*1024*1024,120));setInterval(()=>{},1000)"], { ...options, env: { ...options.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" } }),
    });
    assert.equal(result.success, false);
    assert.match(result.error, /exceeded 8 MiB/);
    assert.equal((await readFile(join(directory, "execution.log"))).length, 8 * 1024 * 1024);
    const uncertain = await prepareBrowser("webkit", { directory, frontend, deadline: Date.now() + 10000, execute: async () => ({ code: 0, cleanupUnverified: true }) });
    assert.equal(uncertain.success, false);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("pinned installer dry run preserves WebKit revision provenance without downloads", { timeout: 15000 }, async () => {
  const { prepareBrowser } = await import("../ci/ui-runner.mjs");
  const { execute } = await import("../validation/stage.mjs");
  const { fileURLToPath } = await import("node:url");
  const directory = await mkdtemp(join(tmpdir(), "mesh-browser-provenance-"));
  const frontend = fileURLToPath(new URL("../../frontend/", import.meta.url));
  let output = "";
  try {
    const result = await prepareBrowser("webkit", { directory, frontend, deadline: Date.now() + 12000, onOutput(chunk) { output += chunk; },
      execute: (command, options) => execute([...command, "--dry-run"], { ...options, env: { ...options.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" } }),
    });
    assert.equal(result.success, true, JSON.stringify(result));
    assert.match(output, /WebKit|webkit/);
    assert.ok([result.preparation.browserRevision, ...Object.values(result.preparation.revisionOverrides)].some(revision => output.includes(String(revision))), output);
    assert.match(output, /https:\/\//);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("preparation fixtures preserve their independent ownership inside the real outer stage", { timeout: 20000 }, async () => {
  const { execute } = await import("../validation/stage.mjs");
  const { fileURLToPath } = await import("node:url");
  const directory = await mkdtemp(join(tmpdir(), "mesh-preparation-outer-"));
  let output = "";
  const originalOwner = process.env.MESH_VALIDATION_PROCESS_GROUP_OWNER;
  try {
    const result = await execute([process.execPath, "--test", "--test-name-pattern=real preparation timeout and cancellation|preparation output has|pinned installer dry", fileURLToPath(import.meta.url)], {
      env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "", NODE_TEST_CONTEXT: undefined }, logPath: join(directory, "outer.log"), onOutput(chunk) { output += chunk; },
    });
    assert.equal(result.code, 0, output);
    assert.ok(!result.cleanupUnverified, JSON.stringify(result));
    assert.match(output, /pass 3/);
    assert.match(output, /cancelled 0/);
    assert.equal(process.env.MESH_VALIDATION_PROCESS_GROUP_OWNER, originalOwner);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
