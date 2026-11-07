import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { mutationOptions, runMutationTool, verifyMutationRetirement } from "./mutation-baseline.mjs";
import { parseSelectedMutants } from "./mutation-evidence.mjs";
import { runCommand } from "../pr-platform/guest.mjs";

test("mutation retirement requires a nonce-bound kernel acknowledgement", () => {
  const result = { code: 0, cancelled: false, timedOut: false };
  const acknowledgement = { schema: 1, nonce: "expected", retired: true, exitCode: 0 };
  verifyMutationRetirement(result, acknowledgement, "expected");
  for (const bad of [undefined, { ...acknowledgement, nonce: "stale" }, { ...acknowledgement, retired: false }, { ...acknowledgement, exitCode: 1 }])
    assert.throws(() => verifyMutationRetirement(result, bad, "expected"));
});

const native = { skip: process.platform !== "linux" && "Linux pidfd/subreaper control", timeout: 150000 };
const alive = pid => { try { process.kill(pid, 0); return true; } catch (error) { if (error.code === "ESRCH") return false; throw error; } };
const waitFor = async (predicate, deadline = 30000) => {
  const until = performance.now() + deadline;
  while (!predicate()) { assert.ok(performance.now() < until, "owned fixture readiness deadline exceeded"); await new Promise(resolve => setTimeout(resolve, 25)); }
};
const ignore = 'unsafe extern "C" { fn signal(n:i32, handler:usize)->usize; } unsafe { signal(15, 1); }';
const publish = 'let marker=std::env::var("MUTATION_NATIVE_MARKER").unwrap(); let pending=format!("{marker}.partial"); std::fs::write(&pending,std::process::id().to_string()).unwrap(); std::fs::rename(pending,marker).unwrap(); loop { std::thread::sleep(std::time::Duration::from_millis(50)); }';
const fixtureManifest = '[package]\nname="mutation-native-control"\nversion="0.1.0"\nedition="2024"\n[workspace]\n[profile.mutants]\ninherits="test"\n';
const fixtureLib = `pub fn complete()->bool { true }\n#[test] fn owns_test() { if std::env::var("MUTATION_NATIVE_KIND").as_deref()==Ok("direct Nextest") || !complete() { ${ignore} ${publish} } }\n`;
const fixtureBuild = `fn main() { println!("cargo:rerun-if-env-changed=MUTATION_NATIVE_KIND"); if std::env::var("MUTATION_NATIVE_KIND").as_deref()==Ok("active Cargo build") { ${ignore} ${publish} } }\n`;

test("real pinned native-control shard selects the marker-arming complete false mutation", { timeout: 50000 }, async () => {
  const directory = mkdtempSync(path.join(tmpdir(), "mutation-native-selection-"));
  try {
    mkdirSync(path.join(directory, "src"));
    writeFileSync(path.join(directory, "Cargo.toml"), fixtureManifest);
    writeFileSync(path.join(directory, "src/lib.rs"), fixtureLib);
    writeFileSync(path.join(directory, "build.rs"), fixtureBuild);
    const result = await runMutationTool(["mutants", "--list", "--json", ...mutationOptions("0/256")], directory, path.join(directory, "evidence"), "selection", 30);
    const selected = parseSelectedMutants(result.stdout, result.code);
    assert.equal(selected.length, 1, JSON.stringify(selected));
    assert.equal(selected[0].file, "src/lib.rs");
    assert.equal(selected[0].replacement, "false");
    assert.equal(selected[0].function.function_name, "complete");
    assert.equal(existsSync(path.join(directory, "target")), false, "listing must not compile the native fixture");
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

for (const kind of ["direct Nextest", "nested cargo-mutants", "active Cargo build"]) {
  test(`Linux kernel owner retires ${kind} ignoring TERM and preserves a sibling`, native, async () => {
    const directory = mkdtempSync(path.join(tmpdir(), "mutation-native-owner-"));
    const saved = { ...process.env };
    let sibling, markerPid, running;
    const controller = new AbortController();
    try {
      mkdirSync(path.join(directory, "src")); mkdirSync(path.join(directory, ".config"));
      writeFileSync(path.join(directory, "Cargo.toml"), fixtureManifest);
      writeFileSync(path.join(directory, ".config/nextest.toml"), '[profile.mutants]\nglobal-timeout="1800s"\nslow-timeout={period="60s",terminate-after=15,grace-period="5s"}\ntest-threads=4\n');
      writeFileSync(path.join(directory, "src/lib.rs"), fixtureLib);
      writeFileSync(path.join(directory, "build.rs"), fixtureBuild);
      delete process.env.MUTATION_NATIVE_KIND;
      delete process.env.MUTATION_NATIVE_MARKER;
      await runMutationTool(["generate-lockfile", "--offline"], directory, path.join(directory, "evidence/setup"), "lock", 30);
      // Compile once before the readiness-bound cancellation experiment.
      await runMutationTool(["nextest", "list", "--locked", "--cargo-profile", "mutants", "--profile", "mutants"], directory, path.join(directory, "evidence/setup"), "compile", 30);
      const marker = path.join(directory, "owned-test.pid");
      Object.assign(process.env, { MUTATION_NATIVE_KIND: kind, MUTATION_NATIVE_MARKER: marker });
      sibling = spawn(process.execPath, ["-e", 'process.on("SIGTERM",()=>{});setInterval(()=>{},1000)'], { stdio: "ignore" });
      const args = kind === "direct Nextest" ? ["nextest", "run", "--locked", "--cargo-profile", "mutants", "--profile", "mutants", "--retries", "0"]
        : ["mutants", ...mutationOptions("0/256"), "--baseline", "skip", "--", "--test-threads=4"];
      running = runMutationTool(args, directory, path.join(directory, "evidence"), "cancel", 60, [0, 2], controller.signal);
      let settled = false, earlyFailure;
      const outcome = running.then(() => { settled = true; return null; }, error => { settled = true; earlyFailure = error; return error; });
      try {
        await waitFor(() => {
          if (existsSync(marker)) return true;
          assert.equal(settled, false, `tool exited before owned fixture readiness: ${earlyFailure || "unexpected success"}`);
          return false;
        });
      } catch (error) {
        const log = path.join(directory, "evidence/cancel.log");
        error.message += `\nBounded mutation phase log tail:\n${existsSync(log) ? readFileSync(log, "utf8").slice(-4096) : "phase log missing"}`;
        throw error;
      }
      markerPid = Number(readFileSync(marker, "utf8"));
      assert.ok(Number.isSafeInteger(markerPid) && markerPid > 1 && alive(markerPid));
      controller.abort();
      assert.match(String(await outcome), /mutation cancel process failed/);
      assert.equal(alive(markerPid), false, "acknowledged fixture must actually be retired");
      assert.equal(alive(sibling.pid), true, "unrelated sibling must remain alive");
      const result = JSON.parse(readFileSync(path.join(directory, "evidence/cancel-result.json"), "utf8"));
      assert.equal(result.cleanupUnverified, false, "kernel acknowledgement must qualify cleanup");
      assert.equal(result.cancelled, true);
      assert.equal(existsSync(path.join(directory, "evidence/receipt.json")), false);
      if (kind !== "direct Nextest") assert.match(readFileSync(path.join(directory, "src/lib.rs"), "utf8"), /complete\(\)->bool \{ true \}/, "cancellation of tests and builds must restore mutated source");
    } finally {
      controller.abort();
      if (running) await running.catch(() => {});
      // Only fixture-owned acknowledged PID and direct sibling, never a scan.
      if (markerPid && alive(markerPid)) process.kill(markerPid, "SIGKILL");
      if (sibling) { sibling.kill("SIGKILL"); await new Promise(resolve => sibling.exitCode !== null || sibling.signalCode !== null ? resolve() : sibling.once("exit", resolve)); }
      for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key];
      Object.assign(process.env, saved);
      rmSync(directory, { recursive: true, force: true });
    }
  });
}

test("Linux normal leader exit retires an adopted detached child and preserves status and sibling", native, async () => {
  const directory = mkdtempSync(path.join(tmpdir(), "mutation-owner-adoption-"));
  let sibling, markerPid;
  try {
    const marker = path.join(directory, "owned.pid");
    const acknowledgement = path.join(directory, "retired.json");
    const nonce = randomBytes(32).toString("hex");
    const descendant = 'import os,signal,time,sys;signal.signal(signal.SIGTERM,signal.SIG_IGN);p=sys.argv[1];open(p+".partial","w").write(str(os.getpid()));os.rename(p+".partial",p);time.sleep(300)';
    const leader = 'import os,subprocess,sys,time;subprocess.Popen([sys.executable,"-c",sys.argv[2],sys.argv[1]],start_new_session=True,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);\nwhile not os.path.exists(sys.argv[1]): time.sleep(.01)\nsys.exit(23)';
    sibling = spawn(process.execPath, ["-e", 'process.on("SIGTERM",()=>{});setInterval(()=>{},1000)'], { stdio: "ignore" });
    const result = await runCommand("python3", [fileURLToPath(new URL("mutation-process-owner.py", import.meta.url)), acknowledgement, nonce, "--", "python3", "-c", leader, marker, descendant], {
      cwd: directory, timeoutMs: 30000, terminationGraceMs: 20000, logPath: path.join(directory, "owner.log"),
    });
    assert.equal(result.code, 23, result.error || result.output);
    assert.equal(result.timedOut, false);
    assert.equal(result.cleanupUnverified, false);
    markerPid = Number(readFileSync(marker, "utf8"));
    assert.ok(Number.isSafeInteger(markerPid) && markerPid > 1);
    verifyMutationRetirement(result, JSON.parse(readFileSync(acknowledgement, "utf8")), nonce);
    assert.equal(alive(markerPid), false, "normal leader exit cannot acknowledge an adopted live descendant");
    assert.equal(alive(sibling.pid), true);
  } finally {
    if (!markerPid && existsSync(path.join(directory, "owned.pid"))) markerPid = Number(readFileSync(path.join(directory, "owned.pid"), "utf8"));
    if (Number.isSafeInteger(markerPid) && markerPid > 1 && alive(markerPid)) process.kill(markerPid, "SIGKILL");
    if (sibling) { sibling.kill("SIGKILL"); await new Promise(resolve => sibling.exitCode !== null || sibling.signalCode !== null ? resolve() : sibling.once("exit", resolve)); }
    rmSync(directory, { recursive: true, force: true });
  }
});

test("Linux missing retirement acknowledgement quarantines successful-looking work", native, async () => {
  const directory = mkdtempSync(path.join(tmpdir(), "mutation-missing-ack-"));
  const savedPath = process.env.PATH;
  try {
    const fakePython = path.join(directory, "python3");
    writeFileSync(fakePython, "#!/bin/sh\nexit 0\n"); chmodSync(fakePython, 0o755);
    process.env.PATH = `${directory}:${savedPath}`;
    await assert.rejects(runMutationTool(["--version"], directory, path.join(directory, "evidence"), "missing", 10), /mutation missing process failed/);
    const result = JSON.parse(readFileSync(path.join(directory, "evidence/missing-result.json"), "utf8"));
    assert.equal(result.cleanupUnverified, true);
    assert.equal(existsSync(path.join(directory, "evidence/receipt.json")), false);
  } finally { process.env.PATH = savedPath; rmSync(directory, { recursive: true, force: true }); }
});

test("Linux postspawn pidfd failure retires its unreaped child without acknowledging success", native, async () => {
  const directory = mkdtempSync(path.join(tmpdir(), "mutation-owner-fault-"));
  let sibling, markerPid;
  try {
    const marker = path.join(directory, "owned.pid"), acknowledgement = path.join(directory, "retired.json");
    const nonce = randomBytes(32).toString("hex");
    const child = 'import os,signal,sys,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);p=sys.argv[1];open(p+".partial","w").write(str(os.getpid()));os.rename(p+".partial",p);time.sleep(300)';
    // Inject a syscall failure in the imported real owner, not a production flag.
    // Wait for actual atomic child readiness before every forced pidfd failure.
    const injection = `import errno,importlib.util,os,sys,time
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location("owner",sys.argv[1]);owner=importlib.util.module_from_spec(spec);spec.loader.exec_module(owner)
original=os.pidfd_open
marker=sys.argv[4]
def fail_child(pid,*args):
    if pid==os.getpid(): return original(pid,*args)
    until=time.monotonic()+5
    while not os.path.exists(marker) and time.monotonic()<until: time.sleep(.01)
    raise OSError(errno.EMFILE,"injected leader pidfd failure")
os.pidfd_open=fail_child
sys.argv=[sys.argv[1],sys.argv[2],sys.argv[3],"--",sys.executable,"-c",sys.argv[5],marker]
try: sys.exit(owner.main())
except Exception as error:
    print(str(error),file=sys.stderr)
    sys.exit(125)
`;
    sibling = spawn(process.execPath, ["-e", 'process.on("SIGTERM",()=>{});setInterval(()=>{},1000)'], { stdio: "ignore" });
    const result = await runCommand("python3", ["-c", injection, fileURLToPath(new URL("mutation-process-owner.py", import.meta.url)), acknowledgement, nonce, marker, child], {
      cwd: directory, timeoutMs: 30000, terminationGraceMs: 20000, logPath: path.join(directory, "fault.log"),
    });
    assert.equal(result.code, 125, result.output);
    assert.match(result.output, /injected leader pidfd failure/);
    markerPid = Number(readFileSync(marker, "utf8"));
    assert.ok(Number.isSafeInteger(markerPid) && markerPid > 1);
    assert.equal(alive(markerPid), false, "known direct child must retire even when every child pidfd acquisition fails");
    assert.equal(alive(sibling.pid), true);
    assert.equal(existsSync(acknowledgement), false, "recovered cleanup cannot erase the original ownership error");
    assert.throws(() => verifyMutationRetirement(result, undefined, nonce), /acknowledgement/);
  } finally {
    if (!markerPid && existsSync(path.join(directory, "owned.pid"))) markerPid = Number(readFileSync(path.join(directory, "owned.pid"), "utf8"));
    if (Number.isSafeInteger(markerPid) && markerPid > 1 && alive(markerPid)) process.kill(markerPid, "SIGKILL");
    if (sibling) { sibling.kill("SIGKILL"); await new Promise(resolve => sibling.exitCode !== null || sibling.signalCode !== null ? resolve() : sibling.once("exit", resolve)); }
    rmSync(directory, { recursive: true, force: true });
  }
});
