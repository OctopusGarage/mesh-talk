import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, writeFile, readFile, rm, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
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
  const run = options => runStage({ root, cwd: root, name: "fixture", command, env: { ...process.env, CI: "" }, log: () => {}, ...options });
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
