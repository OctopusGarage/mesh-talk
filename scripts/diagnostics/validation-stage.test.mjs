import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, writeFile, readFile, rm, mkdir, readdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), "mesh-stage-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const git = args => { const r = spawnSync("git", args, { cwd: root, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); return r.stdout; };
  git(["init", "-q"]);
  await writeFile(path.join(root, ".gitignore"), "target/\ncount\nartifact\n");
  await writeFile(path.join(root, "source"), "one");
  git(["add", "."]); git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "fixture"]);
  const command = [process.execPath, "-e", "const fs=require('node:fs'); const n=fs.existsSync('count')?Number(fs.readFileSync('count')):0; fs.writeFileSync('count',String(n+1)); fs.writeFileSync('artifact','measured');"];
  const { runStage } = await import("../validation/stage.mjs");
  const run = options => runStage({ root, cwd: root, name: "fixture", command, log: () => {}, ...options,
    // Each fixture owns its own group even when this test suite itself runs
    // inside an affected-validation supervisor.
    env: { ...process.env, CI: "", ...options?.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined } });
  const count = async () => Number(await readFile(path.join(root, "count"), "utf8"));
  return { root, git, run, count, command };
}

test("an identical successful command executes once and retains timing evidence", async t => {
  const f = await fixture(t);
  const first = await f.run(); const second = await f.run();
  assert.equal(first.code, 0); assert.equal(first.reused, false); assert.equal(second.reused, true);
  assert.equal(await f.count(), 1);
  assert.ok(first.elapsedMs >= 0); assert.ok(second.receipt);
});

test("changed command, relevant environment, base and revision invalidate successful evidence", async t => {
  const f = await fixture(t);
  await f.run();
  await f.run({ command: [...f.command, "literal $(never execute)"] });
  await f.run({ env: { ...process.env, CI: "", RUST_TEST_THREADS: "3" } });
  await f.run({ context: { base: "one" } }); await f.run({ context: { base: "two" } });
  f.git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "--allow-empty", "-qm", "new revision"]);
  await f.run();
  assert.equal(await f.count(), 6);
});

test("missing or changed output artifacts require execution again", async t => {
  const f = await fixture(t);
  await f.run({ artifacts: ["artifact"] });
  assert.equal((await f.run({ artifacts: ["artifact"] })).reused, true);
  await writeFile(path.join(f.root, "artifact"), "corrupted"); await f.run({ artifacts: ["artifact"] });
  await rm(path.join(f.root, "artifact")); await f.run({ artifacts: ["artifact"] });
  assert.equal(await f.count(), 3);
});

test("failed commands and dirty sources cannot produce reusable success", async t => {
  const f = await fixture(t);
  const command = [process.execPath, "-e", "process.exit(7)"];
  assert.equal((await f.run({ command })).code, 7);
  assert.equal((await f.run({ command })).reused, false);
  await writeFile(path.join(f.root, "source"), "dirty");
  await f.run(); await f.run();
  assert.equal(await f.count(), 2);
});

test("concurrent callers share one successful execution", async t => {
  const f = await fixture(t);
  const results = await Promise.all([f.run(), f.run()]);
  assert.equal(results.filter(r => r.reused).length, 1); assert.equal(await f.count(), 1);
});

test("CI and explicit fresh execution never reuse local verdicts", async t => {
  const f = await fixture(t);
  await f.run(); await f.run({ fresh: true });
  await f.run({ env: { ...process.env, CI: "1" } });
  assert.equal(await f.count(), 3);
});

test("a source change during execution never records a successful verdict", async t => {
  const f = await fixture(t);
  const command = [process.execPath, "-e", "require('node:fs').writeFileSync('source','changed')"];
  const result = await f.run({ command });
  assert.notEqual(result.code, 0);
  assert.match(result.error, /source changed/i);
});

test("missing required artifacts fail even when the process exits successfully", async t => {
  const f = await fixture(t);
  const result = await f.run({ artifacts: ["not-created"] });
  assert.notEqual(result.code, 0); assert.match(result.error, /artifact/i);
});

test("artifact manifests validate the files they reference, not just the manifest", async t => {
  const f = await fixture(t);
  await f.run();
  await writeFile(path.join(f.root, "target/manifest.json"), JSON.stringify(["artifact"]));
  const options = { artifacts: ["@target/manifest.json"] };
  await f.run(options); assert.equal((await f.run(options)).reused, true);
  await rm(path.join(f.root, "artifact")); assert.equal((await f.run(options)).reused, false);
});

test("shared stages ignore transport hosts while preflight evidence binds to them", async t => {
  const f = await fixture(t);
  const environment = host => ({ ...process.env, CI: "", MESH_PR_WINDOWS_HOST: host });
  await f.run({ profile: "shared", env: environment("one") });
  assert.equal((await f.run({ profile: "shared", env: environment("two") })).reused, true);
  await f.run({ env: environment("one") });
  assert.equal((await f.run({ env: environment("two") })).reused, false);
});

test("different heavy stages cannot overlap their critical sections", async t => {
  const f = await fixture(t);
  const command = [process.execPath, "-e", "const fs=require('node:fs'); const file='target/busy'; if(fs.existsSync(file)) process.exit(9); fs.writeFileSync(file,'busy');setTimeout(()=>fs.unlinkSync(file),80)"];
  const results = await Promise.all([f.run({ name: "one", resource: "heavy", command }), f.run({ name: "two", resource: "heavy", command })]);
  assert(results.every(result => result.code === 0));
});

test("cancelling a running process leaves no successful receipt and releases its locks", async t => {
  const f = await fixture(t);
  const controller = new AbortController();
  const command = [process.execPath, "-e", "require('node:fs').writeFileSync('target/started','1');setInterval(()=>{},1000)"];
  const pending = f.run({ signal: controller.signal, command, resource: "heavy" });
  for (let tries = 0; tries < 500; tries++) {
    try { await readFile(path.join(f.root, "target/started")); break; }
    catch { await new Promise(resolve => setTimeout(resolve, 10)); }
    if (tries === 499) assert.fail("child did not start");
  }
  controller.abort(); const result = await pending;
  assert.equal(result.code, 130);
  await assert.rejects(readFile(result.receipt));
  assert.equal((await f.run({ resource: "heavy" })).code, 0);
});

test("cancellation waits for a detached-stdio descendant that ignores SIGTERM", { skip: process.platform === "win32", timeout: 15000 }, async t => {
  const f = await fixture(t);
  const controller = new AbortController();
  let descendant;
  t.after(() => {
    controller.abort();
    if (descendant) { try { process.kill(descendant, "SIGKILL"); } catch {} }
  });
  const descendantCode = "process.on('SIGTERM',()=>{});require('node:fs').writeFileSync('target/descendant',String(process.pid));setInterval(()=>{},1000)";
  const command = [process.execPath, "-e", `require('node:child_process').spawn(process.execPath,['-e',${JSON.stringify(descendantCode)}],{stdio:'ignore'});setInterval(()=>{},1000)`];
  const pending = f.run({ signal: controller.signal, command, resource: "heavy" });
  try {
    for (let tries = 0; tries < 500; tries++) {
      try { descendant = Number(await readFile(path.join(f.root, "target/descendant"), "utf8")); break; }
      catch { await new Promise(resolve => setTimeout(resolve, 10)); }
    }
    assert.ok(descendant, "descendant did not start");
    controller.abort();
    const result = await pending;
    assert.equal(result.code, 130);
    // An orphaned zombie has stopped running, even if the host init has not
    // reaped it yet. Check state so Linux containers do not report it as alive.
    const running = () => {
      const state = spawnSync("ps", ["-p", String(descendant), "-o", "stat="], { encoding: "utf8" });
      return state.status === 0 && state.stdout.trim() && !state.stdout.trim().startsWith("Z");
    };
    for (let tries = 0; tries < 50 && running(); tries++) await new Promise(resolve => setTimeout(resolve, 20));
    assert.equal(Boolean(running()), false, "descendant survived after resource release");
    assert.equal((await f.run({ resource: "heavy" })).code, 0);
  } finally {
    controller.abort();
    await pending.catch(() => {});
  }
});

test("cancellation immediately before spawn prevents command side effects", async t => {
  const f = await fixture(t);
  const controller = new AbortController();
  await assert.rejects(f.run({
    signal: controller.signal,
    log: message => { if (message.startsWith("started after")) controller.abort(); },
  }), /abort/iu);
  await assert.rejects(f.count(), /ENOENT/u);
  assert.equal((await f.run()).code, 0);
});

test("expired evidence and missing logs cannot authorize reuse", async t => {
  const f = await fixture(t);
  const first = await f.run();
  const record = JSON.parse(await readFile(first.receipt, "utf8"));
  record.finished = Date.now() - 86_400_001;
  await writeFile(first.receipt, JSON.stringify(record));
  const second = await f.run(); assert.equal(second.reused, false);
  await rm(second.logPath);
  assert.equal((await f.run()).reused, false);
  assert.equal(await f.count(), 3);
});

test("an interrupted owner does not leave a permanent lock", async t => {
  const f = await fixture(t);
  const first = await f.run();
  const directory = path.join(f.root, "target/validation/locks", first.key);
  await mkdir(directory);
  await writeFile(path.join(directory, "owner.json"), JSON.stringify({ pid: 2147483647 }));
  assert.equal((await f.run()).reused, true);
});

test("changing Cargo configuration invalidates an otherwise identical command", async t => {
  const f = await fixture(t);
  const cargoHome = path.join(f.root, "target/cargo-home");
  await mkdir(cargoHome, { recursive: true });
  const env = { ...process.env, CI: "", CARGO_HOME: cargoHome };
  await f.run({ env });
  assert.equal((await f.run({ env })).reused, true);
  await writeFile(path.join(cargoHome, "config.toml"), "[build]\njobs=2\n");
  assert.equal((await f.run({ env })).reused, false);
});

test("a failed forced rerun invalidates the older successful verdict", async t => {
  const f = await fixture(t);
  const command = [process.execPath, "-e", "if(require('node:fs').existsSync('target/fail'))process.exit(7)"];
  assert.equal((await f.run({ command })).code, 0);
  await writeFile(path.join(f.root, "target/fail"), "fail");
  assert.equal((await f.run({ command, fresh: true })).code, 7);
  const next = await f.run({ command });
  assert.equal(next.reused, false);
  assert.equal(next.code, 7);
});

test("declared inputs reuse clean local evidence across unrelated commits with provenance", async t => {
  const f = await fixture(t);
  const options = { inputs: ["source"], artifacts: ["artifact"] };
  const first = await f.run(options);
  await writeFile(path.join(f.root, "unrelated"), "documentation");
  f.git(["add", "unrelated"]);
  f.git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "unrelated"]);
  const second = await f.run(options);
  assert.equal(second.reused, true);
  assert.equal(second.source, f.git(["rev-parse", "HEAD"]).trim());
  assert.equal(second.reusedFromSource, first.source);
  assert.equal(await f.count(), 1);
});

test("declared input edits, additions and deletions invalidate cross-revision evidence", async t => {
  const f = await fixture(t);
  await mkdir(path.join(f.root, "inputs"));
  await writeFile(path.join(f.root, "inputs/a"), "one");
  const commit = () => {
    f.git(["add", "-A"]);
    f.git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "inputs"]);
  };
  commit();
  const options = { inputs: ["inputs/", "optional"] };
  await f.run(options);
  assert.equal((await f.run(options)).reused, true);
  await writeFile(path.join(f.root, "inputs/a"), "two"); commit();
  assert.equal((await f.run(options)).reused, false);
  await writeFile(path.join(f.root, "inputs/b"), "new"); commit();
  assert.equal((await f.run(options)).reused, false);
  await rm(path.join(f.root, "inputs/a")); commit();
  assert.equal((await f.run(options)).reused, false);
  await writeFile(path.join(f.root, "optional"), "new config"); commit();
  assert.equal((await f.run(options)).reused, false);
  assert.equal(await f.count(), 5);
});

test("input caching preserves CI, dirty-source, artifact and fresh-run safeguards", async t => {
  const f = await fixture(t);
  const inputs = ["source"];
  await f.run({ inputs });
  assert.equal((await f.run({ inputs, env: { ...process.env, CI: "1" } })).reused, false);
  assert.equal((await f.run({ inputs, fresh: true })).reused, false);
  await writeFile(path.join(f.root, "source"), "dirty");
  assert.equal((await f.run({ inputs })).reused, false);
  assert.equal((await f.run({ inputs })).reused, false);
});

test("unsafe or empty input declarations fail before executing commands", async t => {
  const f = await fixture(t);
  for (const inputs of [[], ["../source"], ["/source"], ["source", ""], ["*.rs"], [":(exclude)source"]]) {
    await assert.rejects(f.run({ inputs }), /input/i);
  }
  await assert.rejects(f.count(), /ENOENT/);
});

test("ignored frontend env files and Vite environment variables invalidate input reuse", async t => {
  const f = await fixture(t);
  await mkdir(path.join(f.root, "frontend"));
  await writeFile(path.join(f.root, ".gitignore"), "target/\ncount\nartifact\nfrontend/.env*\n");
  f.git(["add", ".gitignore"]);
  f.git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "ignore environment"]);
  const options = { inputs: ["frontend/"] };
  await f.run(options);
  await writeFile(path.join(f.root, "frontend/.env.local"), "VITE_FLAG=one\n");
  assert.equal((await f.run(options)).reused, false);
  assert.equal((await f.run(options)).reused, true);
  await writeFile(path.join(f.root, "frontend/.env.local"), "VITE_FLAG=two\n");
  assert.equal((await f.run(options)).reused, false);
  assert.equal((await f.run({ ...options, env: { ...process.env, CI: "", VITE_FLAG: "three" } })).reused, false);
});

test("attempt history retains failures and distinguishes reused work", async t => {
  const f = await fixture(t);
  await f.run(); await f.run();
  await f.run({ command: [process.execPath, "-e", "process.exit(7)"] });
  const directory = path.join(f.root, "target/validation/attempts");
  const records = await Promise.all((await readdir(directory)).map(name => readFile(path.join(directory, name), "utf8").then(JSON.parse)));
  assert.equal(records.length, 3);
  assert.equal(records.filter(record => record.code === 7).length, 1);
  assert.equal(records.filter(record => record.reused).length, 1);
  assert.equal(records.find(record => record.reused).executionMs, 0);
  assert(records.every(record => record.platform === process.platform && record.arch === process.arch));
});

test("cross-revision reuse is only registered for deterministic frontend CLI stages", async () => {
  const { inputGroup } = await import("../validation/stage-inputs.mjs");
  assert(inputGroup("frontend", "frontend-build").includes("Cargo.toml"));
  assert.throws(() => inputGroup("frontend", "browser-full"), /not registered/);
  assert.throws(() => inputGroup("typo", "frontend-build"), /unknown/i);
});

test("revision-bound outer stages cannot hide ignored frontend environment changes", async t => {
  const f = await fixture(t);
  await mkdir(path.join(f.root, "frontend"));
  await writeFile(path.join(f.root, ".gitignore"), "target/\ncount\nartifact\nfrontend/.env*\n");
  f.git(["add", ".gitignore"]);
  f.git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "ignore env"]);
  await f.run();
  await writeFile(path.join(f.root, "frontend/.env.local"), "VITE_FLAG=changed\n");
  assert.equal((await f.run()).reused, false);
});

for (const ignoredStdio of [false, true]) test(
  `nested stage cancellation owns its commands through group cleanup${ignoredStdio ? " with ignored-stdio descendants" : ""}`,
  { skip: process.platform === "win32", timeout: 20000 },
  async t => {
    const f = await fixture(t);
    const controller = new AbortController();
    const { fileURLToPath } = await import("node:url");
    const { createHash } = await import("node:crypto");
    const pidfile = path.join(f.root, "target/nested-command-pid");
    let pid;
    t.after(() => {
      controller.abort();
      if (pid) { try { process.kill(pid, "SIGKILL"); } catch {} }
    });
    const worker = `process.on('SIGTERM',()=>{});require('node:fs').writeFileSync(${JSON.stringify(pidfile)},String(process.pid));setInterval(()=>{},1000)`;
    const command = ignoredStdio
      ? `require('node:child_process').spawn(process.execPath,['-e',${JSON.stringify(worker)}],{stdio:'ignore'});setInterval(()=>{},1000)`
      : worker;
    const pending = f.run({ name: "nested-owner", resource: "heavy", signal: controller.signal,
      command: [process.execPath, fileURLToPath(new URL("../validation/stage.mjs", import.meta.url)),
        "nested-child", "--resource", "focused-static", "--", process.execPath, "-e", command] });
    try {
      for (let tries = 0; tries < 500; tries++) {
        try { pid = Number(await readFile(pidfile, "utf8")); break; }
        catch { await new Promise(resolve => setTimeout(resolve, 10)); }
      }
      assert.ok(pid, "nested command did not start");
      controller.abort();
      await new Promise(resolve => setTimeout(resolve, 100));
      // The outer resource lease must cover the grace period, even when the
      // inner command's leader exits and its descendant ignores SIGTERM.
      for (const resource of ["heavy", "focused-static"])
        await readFile(path.join(f.root, ".git/validation-resources", createHash("sha256").update(resource).digest("hex"), "owner.json"));
      const result = await pending;
      assert.equal(result.code, 130);
      const state = spawnSync("ps", ["-p", String(pid), "-o", "stat="], { encoding: "utf8" });
      const running = state.status === 0 && state.stdout.trim() && !state.stdout.trim().startsWith("Z");
      assert.equal(Boolean(running), false, "nested command survived when outer stage resolved and released its resource");
      await assert.rejects(readFile(result.receipt));
      assert.equal((await f.run({ resource: "heavy" })).code, 0);
      assert.equal((await f.run({ resource: "focused-static" })).code, 0);
    } finally {
      controller.abort();
      await pending.catch(() => {});
    }
  },
);

test("stage distinguishes an unavailable executable from an executed failing command", async t => {
  const f = await fixture(t);
  const unavailable = await f.run({ command: [path.join(f.root, "missing-executable")] });
  assert.equal(unavailable.failureKind, "unavailable");
  assert.equal(unavailable.code, 1);
  const failure = await f.run({ command: [process.execPath, "-e", "process.exit(7)"] });
  assert.equal(failure.code, 7);
  assert.notEqual(failure.failureKind, "unavailable");
});

test("stage CLI publishes structured launch and reused results outside receipt identity", async t => {
  const f = await fixture(t);
  const cli = fileURLToPath(new URL("../validation/stage.mjs", import.meta.url));
  const env = { ...process.env, CI: "", MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined };
  const output = path.join(f.root, "target/result.json");
  for (const count of [1, 2]) {
    const result = spawnSync(process.execPath, [cli, "result-cli", "--result", output, "--", process.execPath, "-e", "process.exit(0)"], { cwd: f.root, env, encoding: "utf8", timeout: 10000 });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.equal(JSON.parse(await readFile(output, "utf8")).reused, count === 2);
  }
  const result = spawnSync(process.execPath, [cli, "result-unavailable", "--result", output, "--", path.join(f.root, "missing-executable")], { cwd: f.root, env, encoding: "utf8", timeout: 10000 });
  assert.notEqual(result.status, 0);
  assert.equal(JSON.parse(await readFile(output, "utf8")).failureKind, "unavailable");
});

test("successful command with changed source is invalidated rather than reported as a test failure", async t => {
  const f = await fixture(t);
  const result = await f.run({ command: [process.execPath, "-e", "require('node:fs').writeFileSync('source','changed while validating')"] });
  assert.equal(result.code, 1);
  assert.equal(result.failureKind, "invalidated");
  assert.match(result.error, /Source changed/);
  await assert.rejects(readFile(result.receipt));
});

test("configuration changed during execution cannot publish successful evidence", async t => {
  const f = await fixture(t);
  await mkdir(path.join(f.root, "target"), { recursive: true });
  const config = path.join(f.root, "target/npmrc");
  await writeFile(config, "audit=true\n");
  const result = await f.run({ env: { NPM_CONFIG_USERCONFIG: config }, command: [process.execPath, "-e", "require('node:fs').writeFileSync(process.env.NPM_CONFIG_USERCONFIG,'audit=false\\n')"] });
  assert.notEqual(result.code, 0);
  assert.equal(result.failureKind, "invalidated");
  await assert.rejects(readFile(result.receipt));
});

test("group cleanup waits for runnable descendants and fails closed on unknown state", async () => {
  const { waitForProcessGroup } = await import("../validation/stage.mjs");
  let probes = 0;
  assert.equal(await waitForProcessGroup(123, { probe: () => ++probes < 3, pause: async () => {} }), true);
  assert.equal(probes, 3);
  assert.equal(await waitForProcessGroup(123, { probe: () => { throw Error("ps unavailable"); }, pause: async () => {}, attempts: 2 }), false);
  assert.equal(await waitForProcessGroup(123, { probe: () => true, pause: async () => {}, attempts: 2 }), false);
});

test("quarantined locks and dead nested owners with live groups cannot be reaped", { skip: process.platform === "win32" }, async t => {
  const f = await fixture(t);
  const first = await f.run();
  const directory = path.join(f.root, "target/validation/locks", first.key);
  const group = Number(spawnSync("ps", ["-o", "pgid=", "-p", String(process.pid)], { encoding: "utf8" }).stdout.trim());
  assert.ok(group > 1);
  for (const extra of [{ quarantined: "termination unknown" }, { group }]) {
    await mkdir(directory, { recursive: true });
    await writeFile(path.join(directory, "owner.json"), JSON.stringify({ pid: 2147483647, ...extra }));
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 1200);
    try { await assert.rejects(f.run({ signal: controller.signal }), /abort/i); }
    finally { clearTimeout(timer); }
    assert.deepEqual(JSON.parse(await readFile(path.join(directory, "owner.json"), "utf8")), { pid: 2147483647, ...extra });
    await rm(directory, { recursive: true });
  }
});

test("transient source edits that are restored before completion invalidate evidence", async t => {
  const f = await fixture(t);
  const result = await f.run({ command: [process.execPath, "-e", "const fs=require('node:fs');fs.writeFileSync('source','temporary');setTimeout(()=>fs.writeFileSync('source','one'),30)"] });
  assert.notEqual(result.code, 0);
  assert.equal(result.failureKind, "invalidated");
  await assert.rejects(readFile(result.receipt));
});

test("log-write errors terminate descendants before releasing resources", { skip: process.platform === "win32", timeout: 15000 }, async t => {
  const f = await fixture(t);
  const { Writable } = await import("node:stream");
  const { writeFileSync } = await import("node:fs");
  const descendant = "process.on('SIGTERM',()=>{});require('node:fs').writeFileSync('target/log-descendant',String(process.pid));setInterval(()=>{},1000)";
  const command = [process.execPath, "-e", `process.on('SIGTERM',()=>process.exit(0));const fs=require('node:fs');require('node:child_process').spawn(process.execPath,['-e',${JSON.stringify(descendant)}],{stdio:'ignore'});const timer=setInterval(()=>{if(fs.existsSync('target/log-descendant')){clearInterval(timer);console.log('trigger log write');}},10);setInterval(()=>{},1000)`];
  const result = await f.run({ command, resource: "heavy", createLogStream: filename => {
    writeFileSync(filename, "");
    return new Writable({ write(_chunk, _encoding, callback) { callback(Error("simulated log disk failure")); } });
  } });
  assert.notEqual(result.code, 0);
  assert.match(result.error, /simulated log disk failure/);
  const pid = Number(await readFile(path.join(f.root, "target/log-descendant"), "utf8"));
  const state = spawnSync("ps", ["-p", String(pid), "-o", "stat="], { encoding: "utf8" });
  assert.ok(state.status !== 0 || !state.stdout.trim() || state.stdout.trim().startsWith("Z"));
  assert.equal((await f.run({ resource: "heavy" })).code, 0);
});


test("cancellation at final result delivery removes published success", async t => {
  const f = await fixture(t);
  const controller = new AbortController();
  const result = await f.run({ signal: controller.signal, log: message => {
    if (message.startsWith("passed in")) controller.abort();
  } });
  assert.equal(result.code, 130);
  await assert.rejects(readFile(result.receipt));
});

test("an ignored frontend environment file created during finalization invalidates success", async t => {
  const f = await fixture(t);
  const { writeFileSync } = await import("node:fs");
  await mkdir(path.join(f.root, "frontend"));
  await writeFile(path.join(f.root, ".gitignore"), "target/\ncount\nartifact\nfrontend/.env*\n");
  f.git(["add", ".gitignore"]);
  f.git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "ignore local environment"]);
  const result = await f.run({ log: message => {
    if (message.startsWith("passed in")) writeFileSync(path.join(f.root, "frontend/.env.local"), "VITE_LOCAL=changed\n");
  } });
  assert.equal(result.code, 1);
  assert.equal(result.failureKind, "invalidated");
  await assert.rejects(readFile(result.receipt));
});
