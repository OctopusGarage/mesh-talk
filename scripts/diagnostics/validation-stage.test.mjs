import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, writeFile, readFile, rm, mkdir, readdir, cp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";
import { toolVersionFixture } from "./test-fixtures/tool-versions.mjs";

const signalReadyWorker = "process.on('SIGTERM',()=>{});const fs=require('node:fs');const ready=process.argv[1];fs.writeFileSync(ready+'.pending',String(process.pid));fs.renameSync(ready+'.pending',ready);setInterval(()=>{},1000)";

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

test("Cargo owns default test concurrency and explicit caller limits remain authoritative", async t => {
  const f = await fixture(t);
  await mkdir(path.join(f.root, ".cargo"));
  await mkdir(path.join(f.root, "src"));
  await writeFile(path.join(f.root, ".cargo/config.toml"), '[env]\nRUST_TEST_THREADS = "4"\n');
  await writeFile(path.join(f.root, "Cargo.toml"), '[package]\nname = "stage-thread-policy"\nversion = "0.1.0"\nedition = "2024"\n');
  await writeFile(path.join(f.root, "src/lib.rs"), '#[test]\nfn respects_thread_policy() { assert_eq!(std::env::var("RUST_TEST_THREADS").unwrap(), std::env::var("EXPECTED_THREADS").unwrap()); }\n');
  for (const [limit, expected] of [[undefined, "4"], ["1", "1"]]) {
    const result = await f.run({ command: ["cargo", "test", "--offline"], fresh: true,
      env: { RUST_TEST_THREADS: limit, EXPECTED_THREADS: expected, CARGO_TARGET_DIR: path.join(f.root, "target") } });
    assert.equal(result.code, 0, await readFile(result.logPath, "utf8"));
  }
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
  const leader = "require('node:child_process').spawn(process.execPath,['-e',process.argv[1],process.argv[2]],{stdio:'ignore'});setInterval(()=>{},1000)";
  const command = [process.execPath, "-e", leader, signalReadyWorker, "target/descendant"];
  const pending = f.run({ signal: controller.signal, command, resource: "heavy" });
  try {
    for (let tries = 0; tries < 500; tries++) {
      try { const candidate = Number(await readFile(path.join(f.root, "target/descendant"), "utf8")); if (Number.isSafeInteger(candidate) && candidate > 1) { descendant = candidate; break; } }
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
  }), error => {
    assert.equal(error.name, "AbortError");
    assert.equal(error.stageResult, undefined);
    return true;
  });
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
    const pidfile = path.join(f.root, "target/nested-command-pid-quote\"-apostrophe'-中文-\n");
    let pid;
    t.after(() => {
      controller.abort();
      if (pid) { try { process.kill(pid, "SIGKILL"); } catch {} }
    });
    const leader = "require('node:child_process').spawn(process.execPath,['-e',process.argv[1],process.argv[2]],{stdio:'ignore'});setInterval(()=>{},1000)";
    const command = ignoredStdio
      ? [process.execPath, "-e", leader, signalReadyWorker, pidfile]
      : [process.execPath, "-e", signalReadyWorker, pidfile];
    const pending = f.run({ name: "nested-owner", resource: "heavy", signal: controller.signal,
      command: [process.execPath, fileURLToPath(new URL("../validation/stage.mjs", import.meta.url)),
        "nested-child", "--resource", "focused-static", "--", ...command] });
    try {
      for (let tries = 0; tries < 500; tries++) {
        try { const candidate = Number(await readFile(pidfile, "utf8")); if (Number.isSafeInteger(candidate) && candidate > 1) { pid = candidate; break; } }
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
  const { env, probes } = toolVersionFixture(f.root, { ...process.env, CI: "", MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined });
  const output = path.join(f.root, "target/result.json");
  for (const count of [1, 2]) {
    const result = spawnSync(process.execPath, [cli, "result-cli", "--result", output, "--", process.execPath, "-e", "process.exit(0)"], { cwd: f.root, env, encoding: "utf8", timeout: 10000 });
    assert.equal(result.status, 0, result.stdout + result.stderr + JSON.stringify({ error: result.error?.message, signal: result.signal }));
    assert.equal(JSON.parse(await readFile(output, "utf8")).reused, count === 2);
  }
  const result = spawnSync(process.execPath, [cli, "result-unavailable", "--result", output, "--", path.join(f.root, "missing-executable")], { cwd: f.root, env, encoding: "utf8", timeout: 10000 });
  assert.notEqual(result.status, 0);
  assert.equal(result.error, undefined, result.stdout + result.stderr);
  assert.equal(JSON.parse(await readFile(output, "utf8")).failureKind, "unavailable");
  assert.deepEqual([...new Set((await readFile(probes, "utf8")).trim().split("\n"))].sort(), ["cargo", "npm", "rustc"]);
});

test("stage CLI rejects local result publication failure after a successful child", async t => {
  const f = await fixture(t);
  const cli = fileURLToPath(new URL("../validation/stage.mjs", import.meta.url));
  const { env } = toolVersionFixture(f.root, { ...process.env, CI: "", MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined });
  // The child creates artifact as a file, so it cannot become a result directory.
  // This is deterministic on every platform, unlike chmod-based permissions.
  const result = spawnSync(process.execPath, [cli, "result-write-failure", "--result", path.join(f.root, "artifact/result.json"), "--", ...f.command], {
    cwd: f.root, env, encoding: "utf8", timeout: 60_000,
  });
  assert.equal(result.error, undefined, result.stdout + result.stderr);
  assert.notEqual(result.status, 0, "successful execution cannot hide failed local publication");
  assert.equal(await f.count(), 1, "the actual child completed before result publication failed");
  assert.equal(await readFile(path.join(f.root, "artifact"), "utf8"), "measured");
  assert.match(result.stderr, /artifact/u);
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
  const leader = "process.on('SIGTERM',()=>process.exit(0));const fs=require('node:fs');require('node:child_process').spawn(process.execPath,['-e',process.argv[1],process.argv[2]],{stdio:'ignore'});const timer=setInterval(()=>{if(fs.existsSync(process.argv[2])){clearInterval(timer);console.log('trigger log write');}},10);setInterval(()=>{},1000)";
  const command = [process.execPath, "-e", leader, signalReadyWorker, "target/log-descendant"];
  const result = await f.run({ command, resource: "heavy", createLogStream: filename => {
    writeFileSync(filename, "");
    return new Writable({ write(_chunk, _encoding, callback) { callback(Error("simulated log disk failure")); } });
  } });
  assert.notEqual(result.code, 0);
  assert.match(result.error, /simulated log disk failure/);
  const pid = Number(await readFile(path.join(f.root, "target/log-descendant"), "utf8"));
  assert.ok(Number.isSafeInteger(pid) && pid > 1, "log failure must follow complete descendant readiness");
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

test("unowned non-detached execution is rejected before starting a command", { skip: process.platform === "win32" }, async () => {
  const { execute } = await import("../validation/stage.mjs");
  await assert.rejects(execute([process.execPath, "-e", "process.exit(0)"], { env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" }, detached: false }), /requires an existing managed process-group owner/);
});

test("a stale group-owner marker cannot signal an unrelated process", { skip: process.platform === "win32", timeout: 10000 }, async t => {
  const { spawn } = await import("node:child_process");
  const { execute } = await import("../validation/stage.mjs");
  const unrelated = spawn(process.execPath, ["-e", "process.send('ready');setInterval(()=>{},1000)"], { stdio: ["ignore", "ignore", "ignore", "ipc"] });
  t.after(() => unrelated.kill("SIGKILL"));
  await new Promise(resolve => unrelated.once("message", resolve));
  const controller = new AbortController();
  const result = await execute([process.execPath, "-e", "console.log('ready');setInterval(()=>{},1000)"], { env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: String(unrelated.pid) }, signal: controller.signal, notifyOwnerOnAbort: true, onOutput() { controller.abort(); } });
  assert.equal(result.cleanupUnverified, true);
  assert.equal(unrelated.exitCode, null, "unrelated live process must not receive the owner notification");
  process.kill(unrelated.pid, 0);
});

for (const code of [0, 23]) test(`ordinary leader exit ${code} retires ignored-stdio work before preserving status`, { skip: process.platform === "win32", timeout: 15000 }, async t => {
  const { execute } = await import("../validation/stage.mjs");
  let pid;
  t.after(() => { if (pid) { const group = Number(spawnSync("ps", ["-o", "pgid=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim()); if (group > 1) try { process.kill(-group, "SIGKILL"); } catch {} } });
  const leaf = "process.on('SIGTERM',()=>{});process.send(process.pid);setInterval(()=>{},1000)";
  const leader = "const c=require('node:child_process').spawn(process.execPath,['-e',process.argv[1]],{stdio:['ignore','ignore','ignore','ipc']});c.once('message',pid=>{console.log('LEAF='+pid);process.exit(Number(process.argv[2]))});";
  const result = await execute([process.execPath, "-e", leader, leaf, String(code)], { env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" }, onOutput(chunk) { const match = chunk.toString().match(/LEAF=(\d+)/); if (match) pid = Number(match[1]); } });
  assert.ok(pid, "descendant must acknowledge installed handler before leader exits");
  assert.equal(result.code, code, result.error);
  assert.equal(result.cleanupUnverified, false);
  assert.equal(result.forcedRetirement, true);
  const state = spawnSync("ps", ["-o", "stat=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim();
  assert.ok(!state || state.startsWith("Z"), `successful leader left runnable work: ${state}`);
});

test("clean standalone and managed nested success preserve exit zero", async () => {
  const { execute } = await import("../validation/stage.mjs");
  const env = { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" };
  const direct = await execute([process.execPath, "-e", "process.exit(0)"], { env });
  assert.equal(direct.code, 0);
  const guest = new URL("../pr-platform/guest.mjs", import.meta.url).href;
  const inner = "const {runCommand}=await import(process.argv[1]);const result=await runCommand(process.execPath,['-e','process.exit(0)']);process.exitCode=result.code;";
  const nested = await execute([process.execPath, "--input-type=module", "-e", inner, guest], { env });
  assert.equal(nested.code, 0);
});

test("a hard-killed outer supervisor cannot expose a live owned group to another heavy stage", { skip: process.platform === "win32", timeout: 20000 }, async t => {
  const { spawn } = await import("node:child_process");
  const { setTimeout: pause } = await import("node:timers/promises");
  const f = await fixture(t);
  const ready = path.join(f.root, "target/ready-quote\"-apostrophe'-中文-\n");
  const admitted = path.join(f.root, "target/second-admitted-quote\"-apostrophe'-中文-\n");
  const cli = fileURLToPath(new URL("../validation/stage.mjs", import.meta.url));
  const env = { ...process.env, CI: "1", MESH_VALIDATION_PROCESS_GROUP_OWNER: "" };
  let leaf, group, first, second;
  t.after(() => { first?.kill("SIGKILL"); second?.kill("SIGKILL"); if (group > 1) try { process.kill(-group, "SIGKILL"); } catch {} });
  first = spawn(process.execPath, [cli, "first-hardkill", "--resource", "heavy-hardkill", "--", process.execPath, "-e", signalReadyWorker, ready], { cwd: f.root, env, stdio: "ignore" });
  const stopped = new Promise(resolve => first.once("close", resolve));
  for (let i = 0; i < 500; i++) { try { const candidate = Number(await readFile(ready, "utf8")); if (Number.isSafeInteger(candidate) && candidate > 1) { leaf = candidate; break; } } catch {} await pause(10); }
  assert.ok(leaf, "the real child must acknowledge readiness");
  group = Number(spawnSync("ps", ["-o", "pgid=", "-p", String(leaf)], { encoding: "utf8" }).stdout.trim());
  assert.equal(group, leaf);
  const resourceRoot = path.join(f.root, ".git/validation-resources");
  const ownerFile = path.join(resourceRoot, (await readdir(resourceRoot))[0], "owner.json");
  // Publication may follow target startup; its prior unknown state must remain
  // non-reapable until this actual group identity has been durably written.
  for (let i = 0; i < 500; i++) { const owner = JSON.parse(await readFile(ownerFile, "utf8")); if (owner.group === group) break; if (!owner.retirementRequired) break; await pause(10); }
  const published = JSON.parse(await readFile(ownerFile, "utf8"));
  first.kill("SIGKILL"); await stopped;
  let queued;
  const waiting = new Promise(resolve => { queued = resolve; });
  second = spawn(process.execPath, [cli, "second-hardkill", "--resource", "heavy-hardkill", "--", process.execPath, "-e", "require('node:fs').writeFileSync(process.argv[1],'entered')", admitted], { cwd: f.root, env, stdio: ["ignore", "pipe", "pipe"] });
  const completed = new Promise(resolve => second.once("close", resolve));
  second.stdout.on("data", chunk => { if (chunk.toString().includes("waiting for another validation stage")) queued(); });
  await waiting;
  assert.equal(published.retirementRequired, true, "unknown admission must not be PID-only reapable");
  assert.equal(published.group, group);
  assert.equal(JSON.parse(await readFile(ownerFile, "utf8")).group, group);
  await assert.rejects(readFile(admitted), { code: "ENOENT" });
  assert.doesNotThrow(() => process.kill(leaf, 0));
  process.kill(-group, "SIGKILL");
  assert.equal(await completed, 0, "a provably retired known group must permit recovery");
  assert.equal(await readFile(admitted, "utf8"), "entered");
});


test("a dead owner with unknown admitted group cannot be automatically reaped", { skip: process.platform === "win32", timeout: 10000 }, async t => {
  const f = await fixture(t);
  const first = await f.run();
  const directory = path.join(f.root, "target/validation/locks", first.key);
  await mkdir(directory);
  const owner = { pid: 2147483647, retirementRequired: true };
  await writeFile(path.join(directory, "owner.json"), JSON.stringify(owner));
  const controller = new AbortController();
  let queued = false;
  await assert.rejects(f.run({ signal: controller.signal, log: message => {
    if (message.startsWith("waiting for another validation stage")) {
      queued = true;
      controller.abort();
    }
  } }), { name: "AbortError" });
  assert.equal(queued, true, "real waiter must observe the retained lease");
  assert.deepEqual(JSON.parse(await readFile(path.join(directory, "owner.json"), "utf8")), owner);
  assert.equal(await f.count(), 1, "unknown ownership must not admit another command");
});

test("leader exit retires descendants retaining inherited output pipes", { skip: process.platform === "win32", timeout: 15000 }, async t => {
  const { execute } = await import("../validation/stage.mjs");
  let pid;
  t.after(() => { if (pid) { const group = Number(spawnSync("ps", ["-o", "pgid=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim()); if (group > 1) try { process.kill(-group, "SIGKILL"); } catch {} } });
  const leaf = "process.on('SIGTERM',()=>{});process.send(process.pid);setInterval(()=>{},1000)";
  const leader = "const c=require('node:child_process').spawn(process.execPath,['-e',process.argv[1]],{stdio:['ignore',1,2,'ipc']});c.once('message',pid=>{console.log('LEAF='+pid);process.exit(0)});";
  const result = await execute([process.execPath, "-e", leader, leaf], { env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" }, onOutput(chunk) { const match = chunk.toString().match(/LEAF=(\d+)/); if (match) pid = Number(match[1]); } });
  assert.ok(pid);
  assert.equal(result.code, 0, result.error);
  assert.equal(result.forcedRetirement, true);
  assert.equal(result.cleanupUnverified, false);
  const state = spawnSync("ps", ["-o", "stat=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim();
  assert.ok(!state || state.startsWith("Z"));
});


test("stage attempts record verified forced retirement", { skip: process.platform === "win32", timeout: 15000 }, async t => {
  const f = await fixture(t);
  const leaf = "process.on('SIGTERM',()=>{});process.send('ready');setInterval(()=>{},1000)";
  const leader = "const c=require('node:child_process').spawn(process.execPath,['-e',process.argv[1]],{stdio:['ignore','ignore','ignore','ipc']});c.once('message',()=>process.exit(0));";
  const result = await f.run({ command: [process.execPath, "-e", leader, leaf] });
  assert.equal(result.code, 0);
  assert.equal(result.forcedRetirement, true);
  assert.equal(JSON.parse(await readFile(result.receipt, "utf8")).forcedRetirement, true);
  const attempts = path.join(f.root, "target/validation/attempts");
  const records = await Promise.all((await readdir(attempts)).map(async name => JSON.parse(await readFile(path.join(attempts, name), "utf8"))));
  assert.equal(records.length, 1);
  assert.equal(records[0].forcedRetirement, true);
});


test("ordinary retirement does not trust an empty process-table snapshot", { skip: process.platform === "win32", timeout: 15000 }, async t => {
  const f = await fixture(t);
  const copied = path.join(f.root, "target/private-validation");
  await cp(new URL("../validation/", import.meta.url), copied, { recursive: true });
  const filename = path.join(copied, "stage.mjs");
  const original = await readFile(filename, "utf8");
  const declaration = original.match(/function processGroupRunning\([^)]*\) \{/u)?.[0];
  assert.ok(declaration);
  // A private owner copy deterministically supplies stale empty ps snapshots.
  // The real group/descendant remain unmodified; final acceptance checks its PID.
  await writeFile(filename, original.replace(declaration, declaration + "\n  return false;"));
  const { execute } = await import(pathToFileURL(filename).href);
  let pid;
  t.after(() => { if (pid) { const group = Number(spawnSync("ps", ["-o", "pgid=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim()); if (group > 1) try { process.kill(-group, "SIGKILL"); } catch {} } });
  const leaf = "process.on('SIGTERM',()=>{});process.send(process.pid);setInterval(()=>{},1000)";
  const leader = "const c=require('node:child_process').spawn(process.execPath,['-e',process.argv[1]],{stdio:['ignore','ignore','ignore','ipc']});c.once('message',pid=>{console.log('LEAF='+pid);process.exit(0)});";
  const result = await execute([process.execPath, "-e", leader, leaf], { env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" }, onOutput(chunk) { const match = chunk.toString().match(/LEAF=(\d+)/); if (match) pid = Number(match[1]); } });
  assert.ok(pid);
  assert.equal(result.code, 0, result.error);
  const state = spawnSync("ps", ["-o", "stat=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim();
  assert.ok(!state || state.startsWith("Z"), `stale snapshot left live owned work: ${state}`);
  assert.equal(result.forcedRetirement, true);
});

test("process-group retirement bounds probes and pauses by one monotonic deadline", async () => {
  const { waitForProcessGroup } = await import("../validation/stage.mjs");
  let clock = 0;
  assert.equal(await waitForProcessGroup(123, { now: () => clock, probe: () => { clock += 5001; return false; }, pause: async ms => { clock += ms; } }), false, "a late empty observation cannot certify bounded retirement");
  clock = 0;
  const budgets = [], pauses = [];
  assert.equal(await waitForProcessGroup(123, { now: () => clock,
    probe: (_group, remaining) => { budgets.push(remaining); clock += Math.min(2300, remaining); throw Error("probe timed out"); },
    pause: async ms => { pauses.push(ms); clock += ms; } }), false);
  assert.deepEqual(budgets, [5000, 2600, 200]);
  assert.deepEqual(pauses, [100, 100]);
  assert.equal(clock, 5000);
});

test("verification-only evidence never executes on missing, stale, tampered or busy receipts", async t => {
  const f = await fixture(t);
  const { verifyStageEvidence } = await import("../validation/stage.mjs");
  const options = { root: f.root, cwd: f.root, name: "fixture", command: f.command,
    artifacts: ["artifact"], log: () => {},
    env: { ...process.env, CI: "", MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined } };
  const verify = extra => verifyStageEvidence({ ...options, ...extra });
  await assert.rejects(verify(), /Missing, stale, or invalid/);
  await assert.rejects(readFile(path.join(f.root, "count")), /ENOENT/);
  const first = await f.run({ artifacts: ["artifact"] });
  const original = await readFile(first.receipt, "utf8");
  const successful = await verify();
  assert.equal(successful.reused, true);
  assert.equal(successful.code, 0);
  assert.deepEqual(successful.artifacts, first.artifacts);
  assert.equal(await f.count(), 1);
  const { createHash } = await import("node:crypto");
  const heavyLease = path.join(f.root, ".git/validation-resources", createHash("sha256").update("verification-heavy").digest("hex"));
  await mkdir(heavyLease, { recursive: true });
  assert.equal((await verify({ resource: "verification-heavy" })).reused, true,
    "verification must not acquire even a busy heavy resource lease");
  const stale = JSON.parse(original); stale.finished = Date.now() - 86_400_001;
  await writeFile(first.receipt, JSON.stringify(stale));
  await assert.rejects(verify(), /Missing, stale, or invalid/);
  await writeFile(first.receipt, original);
  await writeFile(path.join(f.root, "artifact"), "tampered");
  await assert.rejects(verify(), /Missing, stale, or invalid/);
  await writeFile(path.join(f.root, "artifact"), "measured");
  const log = await readFile(first.logPath);
  await writeFile(first.logPath, "tampered log");
  await assert.rejects(verify(), /Missing, stale, or invalid/);
  await writeFile(first.logPath, log);
  const lease = path.join(f.root, "target/validation/locks", first.key);
  await mkdir(lease);
  await writeFile(path.join(lease, "owner.json"), JSON.stringify({ pid: process.pid }));
  await assert.rejects(verify(), /busy or has an unresolved lease/);
  // A stale owner must not be reaped by a verification-only request.
  await writeFile(path.join(lease, "owner.json"), JSON.stringify({ pid: 2147483647 }));
  await assert.rejects(verify(), /busy or has an unresolved lease/);
  assert.equal(JSON.parse(await readFile(path.join(lease, "owner.json"))).pid, 2147483647);
  await rm(lease, { recursive: true });
  await writeFile(path.join(f.root, "source"), "dirty");
  await assert.rejects(verify(), /dirty source/);
  await writeFile(path.join(f.root, "source"), "one");
  for (const extra of [{ fresh: true }, { env: { ...options.env, CI: "1" } }, { env: { ...options.env, MESH_VALIDATION_FRESH: "1" } }])
    await assert.rejects(verify(extra), /CI, or fresh execution/);
  await writeFile(path.join(f.root, "source"), "new committed source");
  f.git(["add", "source"]);
  f.git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "source drift"]);
  await assert.rejects(verify(), /Missing, stale, or invalid/);
  assert.equal(await f.count(), 1, "verification failures must never execute the producer");
});

test("verification-only finalization rejects source drift after successful receipt inspection", async t => {
  const f = await fixture(t);
  const { verifyStageEvidence } = await import("../validation/stage.mjs");
  const { writeFileSync } = await import("node:fs");
  await f.run();
  await assert.rejects(verifyStageEvidence({ root: f.root, cwd: f.root, name: "fixture", command: f.command,
    env: { ...process.env, CI: "", MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined },
    log: () => writeFileSync(path.join(f.root, "source"), "changed during verification"),
  }), /changed during validation finalization/);
  assert.equal(await f.count(), 1);
});

test("verification-only leases declare no admitted work and dead readers do not quarantine producers", async t => {
  const f = await fixture(t);
  const { verifyStageEvidence } = await import("../validation/stage.mjs");
  const { readFileSync } = await import("node:fs");
  const first = await f.run();
  const lease = path.join(f.root, "target/validation/locks", first.key);
  let descriptor;
  await verifyStageEvidence({ root: f.root, cwd: f.root, name: "fixture", command: f.command,
    env: { ...process.env, CI: "", MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined },
    log: () => { descriptor = JSON.parse(readFileSync(path.join(lease, "owner.json"), "utf8")); },
  });
  assert.equal(descriptor.noWork, true);
  assert.equal(descriptor.retirementRequired, false);
  assert.equal(descriptor.windowsRetirementRequired, undefined);
  const cancelled = new AbortController();
  await assert.rejects(verifyStageEvidence({ root: f.root, cwd: f.root, name: "fixture", command: f.command,
    env: { ...process.env, CI: "", MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined },
    signal: cancelled.signal, log: () => cancelled.abort(),
  }));
  await assert.rejects(readFile(path.join(lease, "owner.json")), /ENOENT/);
  assert.equal(await f.count(), 1, "cancelled verification admits no child work");
  // Model a verifier dying after publishing its actual no-work descriptor.
  // Normal producer recovery may reap this lease on Windows as well as POSIX.
  await mkdir(lease);
  await writeFile(path.join(lease, "owner.json"), JSON.stringify({ ...descriptor, pid: 2147483647 }));
  const controller = new AbortController();
  const deadline = setTimeout(() => controller.abort(), 10_000);
  try {
    const recovered = await f.run({ fresh: true, signal: controller.signal });
    assert.equal(recovered.code, 0);
    assert.equal(await f.count(), 2);
  } finally { clearTimeout(deadline); }
});

test("stage CLI rejects invalid budgets and persists cancellation at an absolute deadline", { timeout: 30000 }, async t => {
  const f = await fixture(t);
  const cli = fileURLToPath(new URL("../validation/stage.mjs", import.meta.url));
  const env = { ...process.env, CI: "1" }; delete env.MESH_VALIDATION_PROCESS_GROUP_OWNER;
  for (const deadline of ["invalid", String(Date.now() - 1), String(Date.now() + 1e12)]) {
    const launch = path.join(f.root, "target/failed-launch.json");
    const result = spawnSync(process.execPath, [cli, "deadline-invalid", "--deadline", deadline, "--result", launch, "--", ...f.command], { cwd: f.root, env, encoding: "utf8", timeout: 10000 });
    assert.notEqual(result.status, 0); assert.match(result.stderr, /deadline/);
    const failure = JSON.parse(await readFile(launch, "utf8"));
    assert.equal(failure.launched, false); assert.equal(failure.code, 1);
  }
  const receipt = path.join(f.root, "target/deadline-result.json");
  const result = spawnSync(process.execPath, [cli, "deadline-parked", "--deadline", String(Date.now() + 2000), "--result", receipt, "--", process.execPath, "-e", "setInterval(()=>{},1000)"], { cwd: f.root, env, encoding: "utf8", timeout: 15000 });
  assert.equal(result.status, 130, result.stderr);
  const record = JSON.parse(await readFile(receipt, "utf8"));
  assert.equal(record.code, 130); assert.equal(record.cleanupUnverified, false);
  // A long unused deadline must not retain an otherwise finished CLI process.
  const normal = spawnSync(process.execPath, [cli, "deadline-normal", "--deadline", String(Date.now() + 60000), "--", process.execPath, "-e", "process.exit(0)"], { cwd: f.root, env, encoding: "utf8", timeout: 15000 });
  assert.equal(normal.status, 0, normal.stderr);
});


test("pre-admission abort preserves the original rejection without mutating its reason", async t => {
  const f = await fixture(t);
  const controller = new AbortController(); controller.abort();
  await assert.rejects(f.run({ signal: controller.signal }), error => {
    assert.equal(error, controller.signal.reason);
    assert.equal(error.stageResult, undefined);
    return true;
  });
  await assert.rejects(f.count(), /ENOENT/);
  const reader = new AbortController(); reader.abort();
  await assert.rejects(f.run({ reuseOnly: true, signal: reader.signal }), error => {
    assert.equal(error, reader.signal.reason);
    assert.equal(error.stageResult, undefined);
    return true;
  });
});

test("resource-wait abort reports no admission and releases only its own key lease", async t => {
  const f = await fixture(t);
  const { createHash } = await import("node:crypto");
  const resource = "pre-admission-resource";
  const directory = path.join(f.root, ".git/validation-resources", createHash("sha256").update(resource).digest("hex"));
  await mkdir(directory, { recursive: true });
  const owner = { pid: process.pid, started: Date.now() };
  await writeFile(path.join(directory, "owner.json"), JSON.stringify(owner));
  const controller = new AbortController();
  await assert.rejects(f.run({ resource, signal: controller.signal, log: message => {
    if (message.startsWith("waiting for another validation stage")) controller.abort();
  } }), error => {
    assert.equal(error.name, "AbortError");
    assert.equal(error, controller.signal.reason);
    assert.equal(error.stageResult, undefined);
    return true;
  });
  await assert.rejects(f.count(), /ENOENT/);
  assert.deepEqual(await readdir(path.join(f.root, "target/validation/locks")), []);
  assert.deepEqual(JSON.parse(await readFile(path.join(directory, "owner.json"))), owner);
});

test("post-admission exceptions cannot acquire a verified no-work cancellation result", async t => {
  const f = await fixture(t);
  const controller = new AbortController();
  await assert.rejects(f.run({ signal: controller.signal, createLogStream: () => {
    controller.abort(); throw controller.signal.reason;
  } }), error => {
    assert.equal(error, controller.signal.reason);
    assert.equal(error.stageResult, undefined);
    return true;
  });
  await assert.rejects(f.count(), /ENOENT/);
});


test("stage CLI persists owned pre-admission cancellation when cold identity consumes its unchanged 2s budget", { timeout: 30000 }, async t => {
  const f = await fixture(t);
  const preload = path.join(f.root, "target/cold-identity.mjs");
  await mkdir(path.dirname(preload), { recursive: true });
  await writeFile(preload, `
import cp from 'node:child_process';
import {syncBuiltinESMExports} from 'node:module';
const original = cp.spawnSync;
let cold = true;
cp.spawnSync = function(program, args, ...rest) {
  if (cold && args?.length === 1 && args[0] === '--version') {
    cold = false;
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 2100);
  }
  return original.call(this, program, args, ...rest);
};
syncBuiltinESMExports();
`);
  const cli = fileURLToPath(new URL("../validation/stage.mjs", import.meta.url));
  const resultFile = path.join(f.root, "target/cold-result.json");
  const env = { ...process.env, CI: "1", NODE_OPTIONS: `${process.env.NODE_OPTIONS || ""} --import=${pathToFileURL(preload).href}` };
  delete env.MESH_VALIDATION_PROCESS_GROUP_OWNER;
  const result = spawnSync(process.execPath, [cli, "cold-deadline", "--deadline", String(Date.now() + 2000), "--result", resultFile, "--", ...f.command], {
    cwd: f.root, env, encoding: "utf8", timeout: 15000,
  });
  assert.equal(result.status, 130, result.stderr);
  const record = JSON.parse(await readFile(resultFile, "utf8"));
  assert.equal(record.code, 130);
  assert.equal(record.launched, false);
  assert.equal(record.cleanupUnverified, false);
  assert.equal(record.executionMs, 0);
  await assert.rejects(f.count(), /ENOENT/);
});

test("CLI diagnostic failures cannot impersonate an owner-proven pre-admission cancellation", async t => {
  const f = await fixture(t);
  const cli = fileURLToPath(new URL("../validation/stage.mjs", import.meta.url));
  const preload = path.join(f.root, "spoof-owner.mjs");
  await writeFile(preload, "Error.prototype.stageResult = {code:130, launched:false, cleanupUnverified:false};");
  const resultFile = path.join(f.root, "target/diagnostic-result.json");
  const result = spawnSync(process.execPath, [cli, "invalid-profile", "--profile", "unknown", "--result", resultFile, "--", ...f.command], {
    cwd: f.root, encoding: "utf8", timeout: 15000,
    env: { ...process.env, NODE_OPTIONS: `${process.env.NODE_OPTIONS || ""} --import=${pathToFileURL(preload).href}` },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Unknown validation environment profile/);
  const record = JSON.parse(await readFile(resultFile, "utf8"));
  assert.equal(record.code, 1);
  assert.equal(record.launched, null);
  assert.equal(record.cleanupUnverified, true);
  await assert.rejects(f.count(), /ENOENT/);
});

// The leader publishes only its acknowledged child's complete identity. Stream
// chunks never authorize cleanup: stdout may split even a single numeric write.
const escapedPipeLeaf = "process.on('SIGTERM',()=>{});process.send(process.pid);setInterval(()=>{},1000)";
const escapedPipeLeader = "const fs=require('node:fs');const c=require('node:child_process').spawn(process.execPath,['-e',process.argv[1]],{detached:true,stdio:['ignore',1,2,'ipc']});c.once('message',pid=>{try{if(pid!==c.pid||!Number.isSafeInteger(pid)||pid<=1)throw Error('invalid child acknowledgment');const ready=process.argv[2];fs.writeFileSync(ready+'.pending',String(pid)+'\\n');fs.renameSync(ready+'.pending',ready);process.exit(0)}catch(error){const fail=()=>{console.error(error.message);process.exit(1)};if(c.exitCode!==null||c.signalCode!==null){fail();return;}c.once('exit',fail);c.kill('SIGKILL');}});";
async function publishedEscapedPid(filename) {
  const text = await readFile(filename, "utf8");
  assert.match(text, /^[1-9]\d*\n$/, "cleanup requires a complete atomic PID record");
  const pid = Number(text);
  assert.ok(Number.isSafeInteger(pid) && pid > 1, "cleanup requires a safe child PID");
  return pid;
}
async function awaitEscapedPid(filename) {
  const { setTimeout: pause } = await import("node:timers/promises");
  const deadline = Date.now() + 3000;
  while (true) {
    try { return await publishedEscapedPid(filename); }
    catch (error) { if (error.code !== "ENOENT" || Date.now() >= deadline) throw error; }
    await pause(20);
  }
}

for (const cancel of [false, true, "after-drain"]) test(`escaped inherited output pipes fail closed after leader exit${cancel === "after-drain" ? " and cancellation during output finish" : cancel ? " and cancellation" : ""}`, { skip: process.platform === "win32", timeout: 15000 }, async () => {
  const { execute } = await import("../validation/stage.mjs");
  const { Writable } = await import("node:stream");
  const { setTimeout: pause } = await import("node:timers/promises");
  const privateRoot = await mkdtemp(path.join(tmpdir(), "mesh-escaped-pipe-"));
  const readyFile = path.join(privateRoot, "ready");
  const controller = new AbortController();
  let escaped;
  let ownedGroup;
  let cancelledDuringFinish = false;
  let completion;
  const bounded = async (promise, ms, description) => {
    let timer;
    try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(Error(description)), ms); })]); }
    finally { clearTimeout(timer); }
  };
  const running = () => {
    if (!escaped) return false;
    const state = spawnSync("ps", ["-o", "stat=", "-p", String(escaped)], { encoding: "utf8" }).stdout.trim();
    return !!state && !state.startsWith("Z");
  };
  try {
    completion = execute([process.execPath, "-e", escapedPipeLeader, escapedPipeLeaf, readyFile], { env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" }, signal: controller.signal,
      ...(cancel === "after-drain" ? { logPath: "fixture-owned-output", createLogStream: () => new Writable({ write(_chunk, _encoding, done) { done(); }, final(done) {
        // Output finish is entered only after the escaped read ends were closed.
        cancelledDuringFinish = true;
        controller.abort();
        done();
      } }) } : {}),
      onSpawn: pid => { ownedGroup = pid; }, onOutput() {} });
    escaped = await awaitEscapedPid(readyFile);
    assert.ok(Number.isSafeInteger(escaped) && escaped > 1);
    if (cancel === true) {
      await bounded((async () => {
        while (true) {
          try { process.kill(-ownedGroup, 0); }
          catch (error) { if (error.code === "ESRCH") return; throw error; }
          await pause(20);
        }
      })(), 3000, "leader group did not exit after readiness");
      // Exercise cancellation after natural exit has already armed pipe draining.
      await pause(100);
      controller.abort();
    }
    const result = await bounded(completion, 6500, "execute remained pending after leader exit and retirement");
    assert.notEqual(result.code, 0, "escaped pipe holders cannot certify success");
    assert.equal(result.cleanupUnverified, true);
    assert.match(result.error, /output pipes.*retirement/i);
    assert.equal(result.exitCode, 0, "pipe failure preserves the original leader status");
    if (cancel) assert.equal(result.code, 130);
    if (cancel === "after-drain") assert.equal(cancelledDuringFinish, true);
    assert.equal(running(), true, "owner must not signal an unknown escaped group");
  } finally {
    // Recover the atomic record even when readiness or a semantic assertion fails.
    if (!escaped) escaped = await awaitEscapedPid(readyFile);
    if (escaped && running()) process.kill(escaped, "SIGKILL");
    if (completion) await completion;
    for (let attempt = 0; attempt < 50 && running(); attempt++) await pause(20);
    assert.equal(running(), false, "fixture-owned escaped process must retire before completion");
    await rm(privateRoot, { recursive: true, force: true });
  }
});

test("escaped pipe failure records diagnostics and quarantines stage leases without a receipt", { skip: process.platform === "win32", timeout: 15000 }, async t => {
  const { createHash } = await import("node:crypto");
  const { setTimeout: pause } = await import("node:timers/promises");
  const f = await fixture(t);
  const resource = "escaped-pipe-fixture";
  const readyFile = path.join(f.root, "target/escaped-ready");
  const versions = toolVersionFixture(f.root, { ...process.env });
  let escaped;
  let completion;
  let timer;
  const alive = () => {
    if (!escaped) return false;
    const state = spawnSync("ps", ["-o", "stat=", "-p", String(escaped)], { encoding: "utf8" }).stdout.trim();
    return !!state && !state.startsWith("Z");
  };
  try {
    completion = f.run({ name: "escaped-pipe", resource, env: versions.env, command: [process.execPath, "-e", escapedPipeLeader, escapedPipeLeaf, readyFile] });
    const result = await Promise.race([completion, new Promise((_, reject) => { timer = setTimeout(() => reject(Error("stage did not finalize escaped-pipe failure")), 9000); })]);
    escaped = await publishedEscapedPid(readyFile);
    assert.ok(Number.isSafeInteger(escaped) && escaped > 1);
    assert.notEqual(result.code, 0);
    assert.equal(result.cleanupUnverified, true);
    assert.match(result.error, /output pipes.*retirement/i);
    const owner = JSON.parse(await readFile(path.join(f.root, ".git/validation-resources", createHash("sha256").update(resource).digest("hex"), "owner.json"), "utf8"));
    assert.match(owner.quarantined, /output pipes/i);
    await assert.rejects(readFile(result.receipt), { code: "ENOENT" });
    const attemptNames = await readdir(path.join(f.root, "target/validation/attempts"));
    const attempt = JSON.parse(await readFile(path.join(f.root, "target/validation/attempts", attemptNames[0]), "utf8"));
    assert.equal(attempt.cleanupUnverified, true);
    assert.notEqual(attempt.code, 0);
    assert.equal(alive(), true, "quarantine cannot claim the escaped process was killed");
  } finally {
    clearTimeout(timer);
    if (!escaped) escaped = await awaitEscapedPid(readyFile);
    if (Number.isSafeInteger(escaped) && escaped > 1 && alive()) process.kill(escaped, "SIGKILL");
    if (completion) await completion;
    for (let attempt = 0; attempt < 50 && alive(); attempt++) await pause(20);
    assert.equal(alive(), false);
  }
});
