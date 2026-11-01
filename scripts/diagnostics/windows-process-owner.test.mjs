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

async function nativeWorker(t, { hold = false } = {}) {
  const directory = await mkdtemp(path.join(os.tmpdir(), "mesh-windows-owner-"));
  const ready = path.join(directory, "leaf-ready.json");
  const leaf = path.join(directory, "leaf.mjs");
  await writeFile(leaf, `import {writeFileSync} from 'node:fs';writeFileSync(process.argv[2],JSON.stringify({pid:process.pid,token:process.argv[3]}));setInterval(()=>{},1000);setTimeout(()=>process.exit(99),60000);`);
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
  const command = scriptCommand(`& ([ScriptBlock]::Create([IO.File]::ReadAllText(${literal(leader)}))) -Directory ${literal(directory)} -Token ${literal(directory)} -Node ${literal(process.execPath)} ${hold ? "-Hold" : ""}`);
  return { directory, ready, command };
}

test("Windows native leader success retires surviving ignored-stdio descendants", { skip: !windows, timeout: 20000 }, async t => {
  const f = await nativeWorker(t);
  const result = await run(f.command);
  const { pid } = JSON.parse(await readFile(f.ready, "utf8"));
  assert.notEqual(result.code, 0, "leader success cannot certify a surviving native descendant");
  assert.equal(result.exitCode, 0);
  assert.equal(result.cleanupUnverified, false);
  assert.match(result.error, /descendant/i);
  assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
});

test("Windows command preserves exit status and invocation bytes", { skip: !windows, timeout: 15000 }, async () => {
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
  const result = await executeWindows([process.execPath, "-e", `require('node:fs').writeFileSync(${JSON.stringify(ready)},JSON.stringify({pid:process.pid}));setInterval(()=>{},1000)`], {
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
  const source = `require('node:fs').writeFileSync(${JSON.stringify(marker)},'unexpected')`;
  for (const options of [
    { command: [process.execPath, "-e", `${source}\0ignored`] },
    { command: [process.execPath, "-e", source], env: { OWNER_VALUE: "truncated\0ignored" } },
    { command: [process.execPath, "-e", source], cwd: `${root}\0ignored` },
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
