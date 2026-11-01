import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { execute } from "../validation/stage.mjs";

const windows = process.platform === "win32";
const powershell = path.join(process.env.SystemRoot || "C:/Windows", "System32/WindowsPowerShell/v1.0/powershell.exe");
const literal = text => `'${text.replaceAll("'", "''")}'`;
const scriptCommand = text => [powershell, "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(text, "utf16le").toString("base64")];
const run = (command, options = {}) => execute(command, { cwd: process.cwd(), env: process.env, onOutput() {}, ...options });
const leader = fileURLToPath(new URL("./fixtures/windows-native-leader.ps1", import.meta.url));

// These portable cases exercise the actual parent bridge and an actual child
// process. They validate receipt rejection, not Windows kernel retirement.
for (const mode of ["missing", "malformed", "stale", "unconfirmed"]) {
  test(`command owner rejects ${mode} retirement evidence despite helper exit zero`, async t => {
    const { executeWindows } = await import("../validation/windows-process-owner.mjs");
    const { spawn } = await import("node:child_process");
    const directory = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-receipt-"));
    t.after(() => rm(directory, { recursive: true, force: true }));
    const app = path.join(directory, "target.exe");
    await writeFile(app, "Protocol fixture; no native target is launched.\n");
    const fixture = `
      const fs = require('node:fs');
      let input = '';
      process.stdin.setEncoding('utf8');
      process.stdin.on('data', chunk => {
        input += chunk;
        const end = input.indexOf('\\n');
        if (end < 0) return;
        const request = JSON.parse(input.slice(0, end));
        const mode = process.argv[1];
        if (mode !== 'missing') fs.writeFileSync(request.receipt, mode === 'malformed' ? '{' : JSON.stringify({
          schema: 1,
          nonce: mode === 'stale' ? request.nonce + '-previous' : request.nonce,
          result: { retired: mode !== 'unconfirmed', exitCode: 0 },
        }));
        process.exit(0);
      });
    `;
    const result = await executeWindows([app], {
      cwd: process.cwd(), env: process.env, onOutput() {},
      spawnOwner(_program, _args, options) {
        return spawn(process.execPath, ["-e", fixture, mode], options);
      },
    });
    assert.notEqual(result.code, 0);
    assert.equal(result.cleanupUnverified, true);
    assert.match(result.error, /retirement.*receipt|unverified/i);
  });
}

async function nativeWorker(t, { hold = false, exitCode = 0 } = {}) {
  const directory = await mkdtemp(path.join(os.tmpdir(), "mesh-windows-owner-"));
  const ready = path.join(directory, "leaf-ready.json");
  const leaf = path.join(directory, "leaf.mjs");
  await writeFile(leaf, "import {writeFileSync} from 'node:fs';writeFileSync(process.argv[2],JSON.stringify({pid:process.pid,token:process.argv[3]}));setInterval(()=>{},1000);setTimeout(()=>process.exit(99),60000);");
  t.after(async () => {
    // Failed-control cleanup only: retain the actual process handle, then check
    // the unique fixture path before terminating it. Never signal a PID tree.
    let pid; try { pid = JSON.parse(await readFile(ready, "utf8")).pid; } catch { await rm(directory, { recursive: true, force: true }); return; }
    const command = scriptCommand(`$ErrorActionPreference='Stop';$p=Get-Process -Id ${pid} -ErrorAction SilentlyContinue;if($p){try{$null=$p.Handle;$c=Get-CimInstance Win32_Process -Filter 'ProcessId=${pid}';if($c -and ([string]$c.CommandLine).Contains(${literal(leaf)})){$p.Kill();if(!$p.WaitForExit(5000)){throw 'Owned leaf did not retire'}}}finally{$p.Dispose()}};exit 0`);
    const result = spawnSync(command[0], command.slice(1), { encoding: "utf8", timeout: 10000 });
    assert.equal(result.status, 0, result.stderr);
    await rm(directory, { recursive: true, force: true });
  });
  // Native leader avoids Node/libuv's own nested kill-on-close child job,
  // which would conceal the runner's missing descendant ownership.
  const command = scriptCommand(`& ([ScriptBlock]::Create([IO.File]::ReadAllText(${literal(leader)}))) -Directory ${literal(directory)} -Token ${literal(directory)} -Node ${literal(process.execPath)} ${hold ? "-Hold" : ""} -ExitCode ${exitCode}`);
  return { directory, ready, command };
}

test("Windows native leader success retires surviving ignored-stdio descendants", { skip: !windows, timeout: 20000 }, async t => {
  const f = await nativeWorker(t);
  const result = await run(f.command);
  const { pid } = JSON.parse(await readFile(f.ready, "utf8"));
  assert.equal(result.code, 0, result.error);
  assert.equal(result.forcedRetirement, true);
  assert.equal(result.exitCode, 0);
  assert.equal(result.cleanupUnverified, false);
  assert.equal(result.error, undefined);
  assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
});

test("Windows nested runner jobs retire a native descendant before outer success", { skip: !windows, timeout: 20000 }, async t => {
  const f = await nativeWorker(t);
  const guest = new URL("../pr-platform/guest.mjs", import.meta.url).href;
  const planModule = new URL("../pr-platform/plan.mjs", import.meta.url).href;
  const program = `
    import { readFileSync } from 'node:fs';
    const configuration = JSON.parse(process.argv[1]);
    const { runCommand, executePlan } = await import(configuration.guest);
    const { buildPlan, parseFlags } = await import(configuration.planModule);
    const plan = { ...buildPlan(parseFlags([]), []), sourceSha: 'a'.repeat(40), archiveSha: 'b'.repeat(64) };
    let admitted = 0;
    const report = await executePlan(plan, { run: async (program, args, options) => {
      // The first provider step is this real native fixture. Later steps use
      // the actual planned tool and may only enter after its leaf is dead.
      if (admitted++ === 0) return runCommand(configuration.command[0], configuration.command.slice(1), options);
      const { pid } = JSON.parse(readFileSync(configuration.ready, 'utf8'));
      try { process.kill(pid, 0); throw Error('live retired leaf reached next admission'); }
      catch (error) { if (error.code !== 'ESRCH') throw error; }
      return runCommand(program, args, options);
    } });
    if (!report.ok || report.cleanupUnverified || report.results[0].forcedRetirement !== true || admitted !== 3)
      throw Error('Nested native ownership report failed: ' + JSON.stringify(report));
    process.stdout.write('nested retired');
  `;
  const output = [];
  const configuration = JSON.stringify({ guest, planModule, command: f.command, ready: f.ready });
  const result = await run([process.execPath, "--input-type=module", "-e", program, "--", configuration], {
    signal: t.signal,
    onOutput(chunk) { output.push(chunk); },
  });
  assert.equal(result.code, 0, result.error);
  assert.equal(result.cleanupUnverified, false);
  assert.equal(Buffer.concat(output).toString(), "nested retired");
  const { pid } = JSON.parse(await readFile(f.ready, "utf8"));
  assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
});

test("Windows command preserves exit status and invocation bytes", { skip: !windows, timeout: 15000 }, async () => {
  const empty = await run([process.execPath, "-e", "process.exit(0)"]);
  assert.equal(empty.code, 0, empty.error);
  assert.equal(empty.forcedRetirement, true, "empty jobs also accept scope termination");
  assert.equal(empty.cleanupUnverified, false);
  assert.equal((await run([process.execPath, "-e", "process.exit(23)"])).exitCode, 23);
  const args = ["", "a b", "😀中文", 'a"b', "tail\\", 'slash\\\\"quote'];
  const stdout = [], stderr = [];
  const { executeWindows } = await import("../validation/windows-process-owner.mjs");
  const { spawn } = await import("node:child_process");
  const result = await executeWindows([process.execPath, "-e", "process.stdout.write(JSON.stringify({args:process.argv.slice(1),cwd:process.cwd(),secret:process.env.OWNER_TEST_VALUE}));process.stderr.write(Buffer.from([0,255,10,13,128]));", ...args], {
    cwd: process.cwd(), env: { ...process.env, OWNER_TEST_VALUE: "秘密 a b" }, onOutput() {},
    spawnOwner(...argv) {
      assert.equal(argv[1].join(" ").includes("秘密"), false, "environment values must stay off helper argv");
      const child = spawn(...argv);
      child.stdout.on("data", chunk => stdout.push(chunk)); child.stderr.on("data", chunk => stderr.push(chunk));
      return child;
    },
  });
  assert.equal(result.code, 0, result.error);
  assert.equal(result.cleanupUnverified, false);
  assert.deepEqual(JSON.parse(Buffer.concat(stdout)).args, args);
  assert.equal(JSON.parse(Buffer.concat(stdout)).cwd, process.cwd());
  assert.equal(JSON.parse(Buffer.concat(stdout)).secret, "秘密 a b");
  assert.deepEqual([...Buffer.concat(stderr)], [0, 255, 10, 13, 128]);
});

async function waitForFile(filename) {
  const deadline = Date.now() + 10000;
  while (true) {
    try { return JSON.parse(await readFile(filename, "utf8")); }
    catch (error) { if (error.code !== "ENOENT") throw error; }
    assert.ok(Date.now() < deadline, "native fixture readiness not acknowledged");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

test("Windows cancellation retires a native leader and its ignored-stdio descendant", { skip: !windows, timeout: 20000 }, async t => {
  const f = await nativeWorker(t, { hold: true });
  const controller = new AbortController();
  const pending = run(f.command, { signal: controller.signal });
  t.after(() => controller.abort());
  const { pid } = await waitForFile(f.ready);
  controller.abort();
  const result = await pending;
  assert.equal(result.code, 130);
  assert.equal(result.cleanupUnverified, false, result.error);
  assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
});

test("Windows log failure retires the admitted native tree", { skip: !windows, timeout: 20000 }, async t => {
  const { Writable } = await import("node:stream");
  const f = await nativeWorker(t, { hold: true });
  let output;
  const pending = run(f.command, { logPath: "unused", createLogStream() {
    output = new Writable({ write(_chunk, _encoding, done) { done(Error("controlled log failure")); } });
    return output;
  } });
  const { pid } = await waitForFile(f.ready);
  output.write("trigger failure");
  const result = await pending;
  assert.notEqual(result.code, 0);
  assert.match(result.error, /controlled log failure/);
  assert.equal(result.cleanupUnverified, false);
  assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
});

test("Windows helper death cannot manufacture a retirement receipt", { skip: !windows, timeout: 20000 }, async t => {
  const { executeWindows } = await import("../validation/windows-process-owner.mjs");
  const { spawn } = await import("node:child_process");
  const f = await nativeWorker(t, { hold: true });
  let helper;
  const pending = executeWindows(f.command, { cwd: process.cwd(), env: process.env, onOutput() {}, spawnOwner(...argv) { helper = spawn(...argv); return helper; } });
  const { pid } = await waitForFile(f.ready);
  helper.kill(); // Retained ChildProcess handle, never a discovered PID tree.
  const result = await pending;
  assert.notEqual(result.code, 0);
  assert.equal(result.cleanupUnverified, true);
  assert.match(result.error, /receipt/);
  // Kill-on-close containment is a backup, not the owner's missing proof.
  for (let attempt = 0; attempt < 100; attempt++) {
    try { process.kill(pid, 0); } catch { break; }
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
});

test("Windows unavailable executable never falls back to unowned launch", { skip: !windows }, async () => {
  const result = await run(["D:/mesh-owner-does-not-exist.exe"]);
  assert.notEqual(result.code, 0);
  assert.equal(result.failureKind, "unavailable");
  assert.equal(result.cleanupUnverified, false);
});

test("Windows output limit retires owned work without signalling an unrelated process", { skip: !windows, timeout: 20000 }, async t => {
  const { spawn } = await import("node:child_process");
  const { runCommand } = await import("../pr-platform/guest.mjs");
  const unrelated = spawn(process.execPath, ["-e", "setInterval(()=>{},1000)"], { stdio: "ignore" });
  t.after(() => unrelated.kill());
  const result = await runCommand(process.execPath, ["-e", "process.stdout.write('x'.repeat(65536));setInterval(()=>{},1000)"], { timeoutMs: 15000, maxOutputBytes: 32, outputLimitIsFailure: true });
  assert.match(result.error, /output exceeded/);
  assert.equal(result.cleanupUnverified, false);
  assert.equal(result.timedOut, false);
  assert.doesNotThrow(() => process.kill(unrelated.pid, 0));
});

test("Windows dead owner cannot release a lease without retirement evidence", { skip: !windows, timeout: 20000 }, async t => {
  const { mkdir } = await import("node:fs/promises");
  const { runStage } = await import("../validation/stage.mjs");
  const root = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-lock-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const args of [["init", "-q"], ["config", "user.name", "Test"], ["config", "user.email", "test@example.test"]]) {
    assert.equal(spawnSync("git", args, { cwd: root }).status, 0);
  }
  await writeFile(path.join(root, ".gitignore"), "target/\n");
  assert.equal(spawnSync("git", ["add", "."], { cwd: root }).status, 0);
  assert.equal(spawnSync("git", ["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"], { cwd: root }).status, 0);
  const options = { root, name: "native-lock", command: [process.execPath, "-e", "process.exit(0)"], log() {} };
  const first = await runStage(options);
  assert.equal(first.code, 0, first.error);
  const directory = path.join(root, "target/validation/locks", first.key);
  await mkdir(directory);
  const owner = { pid: 2147483647, windowsRetirementRequired: true };
  await writeFile(path.join(directory, "owner.json"), JSON.stringify(owner));
  const controller = new AbortController();
  await assert.rejects(runStage({ ...options, signal: controller.signal, log(message) { if (message.startsWith("waiting for")) controller.abort(); } }), /abort/i);
  assert.deepEqual(JSON.parse(await readFile(path.join(directory, "owner.json"), "utf8")), owner);
});

test("Windows native launch failure returns proof without unowned fallback", { skip: !windows, timeout: 10000 }, async t => {
  const root = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-invalid-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const invalid = path.join(root, "invalid.exe");
  await writeFile(invalid, "not a Windows executable");
  const result = await run([invalid]);
  assert.notEqual(result.code, 0);
  assert.ok(result.error);
  assert.equal(result.cleanupUnverified, false, "failed CreateProcess admitted no work");
});

test("Windows lookup preserves bare names, quoted PATH and duplicate environment selection", { skip: !windows, timeout: 10000 }, async t => {
  const { mkdir, link } = await import("node:fs/promises");
  const root = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-path-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const bin = path.join(root, "quoted bin");
  await mkdir(bin);
  await link(process.execPath, path.join(bin, "owned-node.exe"));
  const output = [];
  const result = await run(["owned-node", "-e", "process.stdout.write(process.env.OWNER_VALUE)"], {
    cwd: root, env: { ...process.env, PATH: `"${bin}"`, Path: "D:/no-such-owned-path", OWNER_VALUE: "first", owner_value: "second" }, onOutput(chunk) { output.push(chunk); },
  });
  assert.equal(result.code, 0, result.error);
  assert.equal(Buffer.concat(output).toString(), "first");
  assert.equal(result.cleanupUnverified, false);
});

test("Windows pre-aborted and startup cancellation cannot leave unowned work", { skip: !windows, timeout: 10000 }, async t => {
  const { executeWindows } = await import("../validation/windows-process-owner.mjs");
  const { spawn } = await import("node:child_process");
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(executeWindows([process.execPath, "-e", "process.exit(0)"], { signal: controller.signal, spawnOwner() { assert.fail("pre-aborted work was admitted"); } }), /abort/i);
  const root = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-startup-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const ready = path.join(root, "ready.json");
  const startup = new AbortController();
  const result = await executeWindows([process.execPath, "-e", "require('node:fs').writeFileSync(process.argv[1],JSON.stringify({pid:process.pid}));setInterval(()=>{},1000)", "--", ready], {
    signal: startup.signal, cwd: root, env: process.env, onOutput() {},
    spawnOwner(...args) { const child = spawn(...args); startup.abort(); return child; },
  });
  assert.equal(result.code, 130);
  assert.equal(result.cleanupUnverified, false, result.error);
  // Admission and cancellation can race, but any admitted process belongs to
  // the job and must retire before return. No PID discovery grants authority.
  try {
    const { pid } = JSON.parse(await readFile(ready, "utf8"));
    assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
  } catch (error) { if (error.code !== "ENOENT") throw error; }
});

test("Windows NUL input cannot launch a truncated command", { skip: !windows }, async t => {
  const { executeWindows } = await import("../validation/windows-process-owner.mjs");
  const root = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-nul-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const marker = path.join(root, "must-not-exist");
  const source = "require('node:fs').writeFileSync(process.argv[1],'unexpected')";
  const nulSource = "require('node:fs').writeFileSync(process.argv[1],'unexpected')\0ignored";
  for (const options of [
    { command: [process.execPath, "-e", nulSource, "--", marker] },
    { command: [process.execPath, "-e", source, "--", marker], env: { OWNER_VALUE: "truncated\0ignored" } },
    { command: [process.execPath, "-e", source, "--", marker], cwd: `${root}\0ignored` },
  ]) {
    const { command, ...input } = options;
    await assert.rejects(executeWindows(command, { ...input, spawnOwner() { assert.fail("NUL input reached helper admission"); } }), { code: "ERR_INVALID_ARG_VALUE" });
  }
  await assert.rejects(readFile(marker), { code: "ENOENT" });
});

test("Windows cancellation during executable preparation does not admit a helper", { skip: !windows }, async () => {
  const { executeWindows } = await import("../validation/windows-process-owner.mjs");
  const controller = new AbortController();
  const pending = executeWindows([process.execPath, "-e", "process.exit(0)"], { signal: controller.signal, spawnOwner() { assert.fail("cancelled preparation admitted a helper"); } });
  controller.abort();
  await assert.rejects(pending, /abort/i);
});

test("Windows cancellation retains ownership through delayed native output drain", { skip: !windows, timeout: 15000 }, async () => {
  const { executeWindows } = await import("../validation/windows-process-owner.mjs");
  const { spawn } = await import("node:child_process");
  const controller = new AbortController();
  let paused = false, resumed = false;
  const result = await executeWindows([process.execPath, "-e", "process.stdout.write('x'.repeat(4*1024*1024));setInterval(()=>{},1000)"], {
    cwd: process.cwd(), env: process.env, signal: controller.signal, onOutput() {},
    spawnOwner(...args) {
      const child = spawn(...args);
      child.stdout.once("data", () => {
        child.stdout.pause(); paused = true;
        controller.abort();
        setTimeout(() => { resumed = true; child.stdout.resume(); }, 3500);
      });
      return child;
    },
  });
  assert.equal(paused, true, "actual helper output must reach the blocked reader");
  assert.equal(resumed, true, "retirement receipt must follow the delayed drain");
  assert.equal(result.code, 130);
  assert.equal(result.cleanupUnverified, false, result.error);
});

test("Windows declared-input receipts bind every executor implementation file", { skip: !windows, timeout: 30000 }, async t => {
  const { mkdir, copyFile, appendFile } = await import("node:fs/promises");
  const { pathToFileURL } = await import("node:url");
  const root = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-cache-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const implementation = path.join(root, "implementation");
  await mkdir(implementation);
  const names = ["stage.mjs", "stage-inputs.mjs", "windows-process-owner.mjs", "windows-process-owner.ps1", "windows-process-owner.cs"];
  for (const name of names) await copyFile(fileURLToPath(new URL(`../validation/${name}`, import.meta.url)), path.join(implementation, name));
  await writeFile(path.join(root, ".gitignore"), "target/\nimplementation/\n");
  await writeFile(path.join(root, "source"), "unchanged declared source");
  for (const args of [["init", "-q"], ["add", "."], ["-c", "commit.gpgsign=false", "-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "fixture"]]) {
    assert.equal(spawnSync("git", args, { cwd: root }).status, 0);
  }
  const { runStage } = await import(pathToFileURL(path.join(implementation, "stage.mjs")));
  const options = { root, name: "declared-native", inputs: ["source"], env: { ...process.env, CI: "" }, command: [process.execPath, "-e", "process.exit(0)"], log() {} };
  const first = await runStage(options);
  assert.equal(first.code, 0, first.error);
  assert.equal((await runStage(options)).reused, true);
  for (const name of names.slice(2)) {
    await appendFile(path.join(implementation, name), name.endsWith("ps1") ? "\n# fixture implementation change\n" : "\n// fixture implementation change\n");
    const result = await runStage(options);
    assert.equal(result.reused, false, `${name} changed outside the declared source input`);
    assert.equal(result.code, 0, result.error);
  }
});

test("unavailable command retains its declared diagnostic log without helper admission", async t => {
  const { executeWindows } = await import("../validation/windows-process-owner.mjs");
  const root = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-unavailable-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const logPath = path.join(root, "unavailable.log");
  const result = await executeWindows([path.join(root, "missing.exe")], {
    cwd: root, env: process.env, logPath,
    spawnOwner() { assert.fail("unavailable target admitted helper work"); },
  });
  assert.equal(result.code, 1);
  assert.equal(result.failureKind, "unavailable");
  assert.equal(result.cleanupUnverified, false);
  assert.equal(await readFile(logPath, "utf8"), "");
});

test("Windows runStage publishes structured unavailable evidence with its log", { skip: !windows, timeout: 15000 }, async t => {
  const { runStage } = await import("../validation/stage.mjs");
  const { createHash } = await import("node:crypto");
  const root = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-unavailable-stage-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await writeFile(path.join(root, ".gitignore"), "target/\n");
  for (const args of [["init", "-q"], ["add", "."], ["-c", "commit.gpgsign=false", "-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "fixture"]]) {
    assert.equal(spawnSync("git", args, { cwd: root }).status, 0);
  }
  const result = await runStage({ root, name: "unavailable-native", command: [path.join(root, "missing.exe")], log() {} });
  assert.equal(result.code, 1);
  assert.equal(result.failureKind, "unavailable");
  assert.equal(result.cleanupUnverified, false);
  const bytes = await readFile(result.logPath);
  assert.equal(bytes.length, 0);
  assert.equal(result.logDigest, createHash("sha256").update(bytes).digest("hex"));
});

test("Windows Git launcher completion is not a runnable-descendant failure", { skip: !windows, timeout: 10000 }, async () => {
  const output = [];
  const result = await run(["git", "--version"], { onOutput(chunk) { output.push(chunk); } });
  assert.equal(result.code, 0, result.error);
  assert.equal(result.exitCode, 0);
  assert.equal(result.cleanupUnverified, false);
  assert.match(Buffer.concat(output).toString(), /^git version /u);
});

test("Windows cold and cached Cargo commands finish real tests after owned-scope retirement", { skip: !windows, timeout: 60000 }, async t => {
  const { mkdir } = await import("node:fs/promises");
  const { runCommand } = await import("../pr-platform/guest.mjs");
  const root = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-cargo-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(path.join(root, "src"));
  await writeFile(path.join(root, "Cargo.toml"), '[package]\nname="owned-command-fixture"\nversion="0.1.0"\nedition="2024"\n');
  await writeFile(path.join(root, "src/lib.rs"), '#[test]\nfn owned_actual_assertion() { assert_eq!(2 + 2, 4); }\n');
  const env = { ...process.env, CARGO_HOME: path.join(root, "cargo"), CARGO_TARGET_DIR: path.join(root, "target") };
  for (const phase of ["cold", "cached"]) {
    const result = await runCommand("cargo", ["test", "--offline", "--message-format=json"], { cwd: root, env, signal: t.signal });
    assert.equal(result.code, 0, `${phase}: ${result.error || result.output}`);
    assert.equal(result.cleanupUnverified, false);
    assert.equal(result.error, undefined);
    assert.equal(typeof result.forcedRetirement, "boolean");
    t.diagnostic(`${phase}: forcedRetirement=${result.forcedRetirement}, cleanupUnverified=${result.cleanupUnverified}`);
    assert.match(result.output, /test result: ok\. 1 passed; 0 failed;/u);
    const build = result.stdout.split(/\r?\n/u).filter(line => line.startsWith('{')).map(line => JSON.parse(line)).find(row => row.reason === "build-finished");
    assert.equal(build?.success, true);
  }
});

test("Windows nonzero leader status survives verified descendant retirement", { skip: !windows, timeout: 20000 }, async t => {
  const { runCommand } = await import("../pr-platform/guest.mjs");
  const f = await nativeWorker(t, { exitCode: 7 });
  const result = await runCommand(f.command[0], f.command.slice(1), { signal: t.signal });
  assert.equal(result.code, 7);
  assert.equal(result.error, undefined);
  assert.equal(result.cleanupUnverified, false);
  assert.equal(result.forcedRetirement, true);
  const { pid } = JSON.parse(await readFile(f.ready, "utf8"));
  assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
});

test("command stream channels preserve structured stdout across stderr fragments", { timeout: 15000 }, async t => {
  const { runCommand } = await import("../pr-platform/guest.mjs");
  const root = await mkdtemp(path.join(os.tmpdir(), "mesh-owner-streams-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const logPath = path.join(root, "producer.log");
  const program = `
    const { readFile } = require('node:fs/promises');
    const { setImmediate } = require('node:timers/promises');
    const log = process.argv[1];
    async function visible(fragment) {
      while (true) {
        try { if ((await readFile(log)).includes(fragment)) return; }
        catch (error) { if (error.code !== 'ENOENT') throw error; }
        await setImmediate();
      }
    }
    (async () => {
      process.stderr.write('progress-prefix ');
      await visible(Buffer.from('progress-prefix '));
      process.stdout.write('{"reason":"build-finished","success":true,"text":"');
      process.stdout.write(Buffer.from([0xf0, 0x9f]));
      await visible(Buffer.from([0xf0, 0x9f]));
      process.stderr.write('progress-mid ');
      await visible(Buffer.from('progress-mid '));
      process.stdout.write(Buffer.from([0x98, 0x80]));
      process.stdout.write('"}\\n');
      await visible(Buffer.from('"}\\n'));
      process.stderr.write('progress-end\\n');
    })().catch(error => { console.error(error); process.exitCode = 1; });
  `;
  const result = await runCommand(process.execPath, ["-e", program, "--", logPath], { logPath, timeoutMs: 10000 });
  assert.equal(result.code, 0, result.error);
  assert.equal(result.cleanupUnverified, false);
  assert.equal(typeof result.stdout, "string");
  assert.equal(result.outputTruncated, false);
  assert.deepEqual(JSON.parse(result.stdout), { reason: "build-finished", success: true, text: "😀" });
  assert.equal(result.stderr, "progress-prefix progress-mid progress-end\n");
  assert.match(result.output, /progress-prefix/u);
  assert.match(result.output, /progress-mid/u);
});

test("command stream capture shares one byte window and retains the output-limit failure", { timeout: 15000 }, async () => {
  const { runCommand } = await import("../pr-platform/guest.mjs");
  const program = "process.stdout.write('a'.repeat(4096));process.stderr.write('b'.repeat(4096))";
  const result = await runCommand(process.execPath, ["-e", program], { maxOutputBytes: 64 });
  assert.equal(result.code, 0, result.error);
  assert.equal(typeof result.stdout, "string");
  assert.equal(typeof result.stderr, "string");
  assert.equal(result.outputTruncated, true);
  assert.ok(Buffer.byteLength(result.output) <= 64);
  assert.ok(Buffer.byteLength(result.stdout) + Buffer.byteLength(result.stderr) <= 64);
  const limited = await runCommand(process.execPath, ["-e", program], { maxOutputBytes: 64, outputLimitIsFailure: true });
  assert.notEqual(limited.code, 0);
  assert.match(limited.error, /output exceeded/u);
  assert.equal(limited.outputTruncated, true);
  assert.equal(limited.cleanupUnverified, false);
});

test("machine output requires an explicitly complete stdout capture", async () => {
  const { requireCommandStdout } = await import("../pr-platform/guest.mjs");
  const output = '{"reason":"build-finished","success":true}\n';
  for (const result of [
    { output, outputTruncated: false },
    { output, stdout: output },
    { output, stdout: output, outputTruncated: true },
  ]) assert.throws(() => requireCommandStdout(result), /stdout|truncat|unverified/i);
  assert.equal(requireCommandStdout({ output: "human log", stdout: output, outputTruncated: false }), output);
});

test("tiny positive output limits bound the combined human log and shared stream window", { timeout: 15000 }, async () => {
  const { runCommand } = await import("../pr-platform/guest.mjs");
  const observations = [];
  for (const cap of [1, 2, 3]) {
    const result = await runCommand(process.execPath, ["-e", "process.stdout.write('x'.repeat(100))"], { maxOutputBytes: cap });
    assert.equal(result.code, 0, result.error);
    assert.equal(result.outputTruncated, true);
    assert.ok(Buffer.byteLength(result.stdout) + Buffer.byteLength(result.stderr) <= cap);
    observations.push({ cap, humanBytes: Buffer.byteLength(result.output) });
  }
  assert.deepEqual(observations.map(({ cap, humanBytes }) => humanBytes <= cap), [true, true, true], JSON.stringify(observations));
});
