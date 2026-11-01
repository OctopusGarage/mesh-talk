import { test } from "node:test";
import assert from "node:assert/strict";

test("backend evidence rejects successful Cargo invocations that ran zero tests", async () => {
  const { validateBackendOutput } = await import("../evals/backend-runner.mjs");
  assert.throws(() => validateBackendOutput("test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out", ["actual_flow"]), /missing|zero|count/i);
});
test("backend evidence requires every expected scenario exactly once, not ignored", async () => {
  const { validateBackendOutput } = await import("../evals/backend-runner.mjs");
  const valid = "test actual_flow ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
  assert.deepEqual(validateBackendOutput(valid, ["actual_flow"]), ["actual_flow"]);
  for (const output of [valid.replace("... ok", "... ignored"), valid + "\ntest actual_flow ... ok", valid.replace("0 failed", "1 failed"), valid.replace("1 passed", "2 passed"), valid.replace("0 filtered out", "2 filtered out")]) assert.throws(() => validateBackendOutput(output, ["actual_flow"]));
});
test("backend evaluates every independent suite after a failure and retains a failing verdict", async () => {
  const { BACKEND_SUITES, evaluateBackendSuites } = await import("../evals/backend-runner.mjs");
  const attempted = [], recorded = {};
  const failures = await evaluateBackendSuites(async target => {
    attempted.push(target);
    const expected = BACKEND_SUITES[target];
    if (target === "post_office_offline") return { code: 101, output: "relay regression", signal: null };
    return { code: 0, signal: null, output: expected.map(name => `test ${name} ... ok`).join("\n") + `\ntest result: ok. ${expected.length} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out` };
  }, async (target, record) => { recorded[target] = record; });
  assert.deepEqual(attempted, Object.keys(BACKEND_SUITES));
  assert.equal(failures.length, 1);
  assert.equal(recorded.post_office_offline.passed, false);
  assert.match(recorded.post_office_offline.failure, /failed/);
  assert.equal(recorded.channel_and_file_cli.passed, true);
  assert.equal(recorded.channel_and_file_cli.scenarios.length, 2);
});

test("filtered large-folder acceptance rejects zero, duplicate, failed and skipped execution", async () => {
  const { LARGE_FOLDER_SCENARIO, validateBackendOutput } = await import("../evals/backend-runner.mjs");
  assert.equal(LARGE_FOLDER_SCENARIO, "large_directory_enqueue_uses_compact_manifest_instead_of_frame_error");
  const valid = `test ${LARGE_FOLDER_SCENARIO} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`;
  const options = { allowFiltered: true };
  assert.deepEqual(validateBackendOutput(valid, [LARGE_FOLDER_SCENARIO], options), [LARGE_FOLDER_SCENARIO]);
  assert.deepEqual(validateBackendOutput(valid.replace("0 filtered out", "1 filtered out"), [LARGE_FOLDER_SCENARIO], options), [LARGE_FOLDER_SCENARIO]);
  for (const output of [valid.split("\n")[1].replace("1 passed", "0 passed"), valid + `\ntest ${LARGE_FOLDER_SCENARIO} ... ok`, valid + `\n${valid.split("\n")[1]}`, valid.replace("0 failed", "1 failed"), valid.replace("... ok", "... ignored"), valid.replace(LARGE_FOLDER_SCENARIO, "other_case"), valid.replace(LARGE_FOLDER_SCENARIO, `node::delivery_runtime_tests::${LARGE_FOLDER_SCENARIO}`), valid.replace(LARGE_FOLDER_SCENARIO, `${LARGE_FOLDER_SCENARIO}_unrelated`)]) {
    assert.throws(() => validateBackendOutput(output, [LARGE_FOLDER_SCENARIO], options));
  }
});

test("instrumented backend execution preserves the exact ignored scenario selection", async () => {
  const { backendCargoArgs } = await import("../evals/backend-runner.mjs");
  const ordinary = backendCargoArgs("two_node_cli", false);
  const covered = backendCargoArgs("two_node_cli", true);
  assert.deepEqual(ordinary.slice(0, 2), ["test", "--locked"]);
  assert.deepEqual(covered.slice(0, 3), ["llvm-cov", "--no-report", "--locked"]);
  assert.deepEqual(covered.slice(3), ordinary.slice(2));
  assert.deepEqual(covered.slice(-4), ["--", "--ignored", "--test-threads=1", "--nocapture"]);
  assert.throws(() => backendCargoArgs("unknown", true), /Unknown backend/);
});

for (const mode of ["output cap", "timeout"]) test(`backend ${mode} retires ignored-stdio workers before the next suite`, { skip: process.platform === "win32", timeout: 15000 }, async t => {
  const { runCargo } = await import("../evals/backend-runner.mjs");
  assert.equal(typeof runCargo, "function");
  const { mkdtemp, readFile, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { spawnSync } = await import("node:child_process");
  const directory = await mkdtemp(join(tmpdir(), "mesh-backend-tree-"));
  const marker = join(directory, "ready"); let pid;
  t.after(async () => { if (pid) try { process.kill(pid, "SIGKILL"); } catch {} await rm(directory, { recursive: true, force: true }); });
  const leaf = `process.on('SIGTERM',()=>{});require('node:fs').writeFileSync(${JSON.stringify(marker)},String(process.pid));process.send('ready');setInterval(()=>{},1000)`;
  const leader = `process.on('SIGTERM',()=>process.exit(0));const c=require('node:child_process').spawn(process.execPath,['-e',${JSON.stringify(leaf)}],{stdio:['ignore','ignore','ignore','ipc']});c.once('message',()=>process.stdout.write('x'.repeat(2048)));setInterval(()=>{},1000)`;
  const result = await runCargo("two_node_cli", { command: [process.execPath, "-e", leader], maxOutputBytes: mode === "output cap" ? 1024 : 4096, timeoutMs: 1500 });
  pid = Number(await readFile(marker, "utf8"));
  assert.match(result.failure, mode === "output cap" ? /output.*limit/i : /timeout/i);
  const state = spawnSync("ps", ["-o", "stat=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim();
  assert.ok(!state || state.startsWith("Z"), `next suite would overlap a runnable worker: ${state}`);
});

test("backend cancellation and unverified cleanup stop admission of later suites", async () => {
  const { evaluateBackendSuites } = await import("../evals/backend-runner.mjs");
  for (const reason of [{ cancelled: true }, { cleanupUnverified: true }]) {
    let calls = 0;
    const failures = await evaluateBackendSuites(async () => { calls++; return { code: 130, failure: "cancelled or unretired work", ...reason }; }, async () => {});
    assert.equal(calls, 1);
    assert.equal(failures.length, 1);
  }
});
