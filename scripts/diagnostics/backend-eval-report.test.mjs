import { test } from "node:test";
import assert from "node:assert/strict";

const capturedOutput = (stdout, stderr = "") => ({ stdout, stderr, output: stdout + stderr, outputTruncated: false });
const suiteOutput = expected => expected.map(name => `test ${name} ... ok`).join("\n") + `\ntest result: ok. ${expected.length} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`;

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
    if (target === "post_office_offline") return { code: 101, ...capturedOutput("", "relay regression"), signal: null };
    return { code: 0, signal: null, ...capturedOutput(suiteOutput(expected)) };
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
  const marker = join(directory, "ready-quote\"-apostrophe'-中文-\n"); let pid;
  t.after(async () => { if (pid) try { process.kill(pid, "SIGKILL"); } catch {} await rm(directory, { recursive: true, force: true }); });
  const leaf = "process.on('SIGTERM',()=>{});require('node:fs').writeFileSync(process.argv[1],String(process.pid));process.send('ready');setInterval(()=>{},1000)";
  const leader = "process.on('SIGTERM',()=>process.exit(0));const c=require('node:child_process').spawn(process.execPath,['-e',process.argv[1],process.argv[2]],{stdio:['ignore','ignore','ignore','ipc']});c.once('message',()=>process.stdout.write('x'.repeat(2048)));setInterval(()=>{},1000)";
  const result = await runCargo("two_node_cli", { command: [process.execPath, "-e", leader, leaf, marker], maxOutputBytes: mode === "output cap" ? 1024 : 4096, timeoutMs: 1500, env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" } });
  pid = Number(await readFile(marker, "utf8"));
  assert.match(result.failure, mode === "output cap" ? /output.*limit/i : /timeout/i);
  const state = spawnSync("ps", ["-o", "stat=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim();
  assert.ok(!state || state.startsWith("Z"), `next suite would overlap a runnable worker: ${state}`);
});

test("backend cancellation and unverified cleanup stop admission of later suites", async () => {
  const { evaluateBackendSuites } = await import("../evals/backend-runner.mjs");
  for (const reason of [{ cancelled: true }, { cleanupUnverified: true }]) {
    let calls = 0;
    const failures = await evaluateBackendSuites(async () => { calls++; return { code: 130, ...capturedOutput(""), failure: "cancelled or unretired work", ...reason }; }, async () => {});
    assert.equal(calls, 1);
    assert.equal(failures.length, 1);
  }
});

test("intentional lifecycle fault fixtures cannot cancel their real validation owner", { skip: process.platform === "win32", timeout: 30000 }, async t => {
  const { spawnSync } = await import("node:child_process");
  const { mkdtemp, writeFile, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { fileURLToPath } = await import("node:url");
  const directory = await mkdtemp(join(tmpdir(), "mesh-managed-faults-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const git = args => { const r = spawnSync("git", args, { cwd: directory, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); };
  git(["init", "-q"]);
  await writeFile(join(directory, ".gitignore"), "target/\n");
  git(["add", ".gitignore"]);
  git(["-c", "commit.gpgsign=false", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-qm", "fixture"]);
  const files = ["backend-eval-report.test.mjs", "pr-platform.test.mjs", "pr-preflight-output.test.mjs"].map(name => fileURLToPath(new URL(name, import.meta.url)));
  // A fresh test driver must not inherit node:test's child-context marker,
  // which otherwise silently suppresses execution of these nested test files.
  const result = spawnSync(process.execPath, [fileURLToPath(new URL("../validation/stage.mjs", import.meta.url)), "fault-fixtures", "--", process.execPath, "--test", "--test-reporter=tap", "--test-name-pattern", "backend (output cap|timeout)|bounded child execution|browser wrapper exposes", ...files], { cwd: directory, env: { ...process.env, CI: "1", MESH_VALIDATION_PROCESS_GROUP_OWNER: "", NODE_TEST_CONTEXT: undefined }, encoding: "utf8", timeout: 25000 });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.match(result.stdout, /browser wrapper exposes/);
  assert.match(result.stdout, /backend timeout/);
  assert.match(result.stdout, /^# pass 4$/mu, "all four selected fault controls must actually pass");
});


test("backend scenarios require complete stdout instead of merged or stderr evidence", async () => {
  const { BACKEND_SUITES, evaluateBackendSuites } = await import("../evals/backend-runner.mjs");
  const variants = [
    valid => capturedOutput("", valid),
    valid => { const result = capturedOutput(valid); delete result.stdout; return result; },
    valid => ({ ...capturedOutput(valid), outputTruncated: true }),
    valid => { const result = capturedOutput(valid); delete result.outputTruncated; return result; },
    valid => { const split = valid.indexOf("test result:") + "test result:".length; return capturedOutput(valid.slice(0, split), valid.slice(split)); },
  ];
  for (const evidence of variants) {
    const recorded = {};
    const failures = await evaluateBackendSuites(async target => ({ code: 0,
      ...evidence(suiteOutput(BACKEND_SUITES[target])) }), async (target, record) => { recorded[target] = record; });
    assert.equal(failures.length, Object.keys(BACKEND_SUITES).length);
    for (const record of Object.values(recorded)) {
      assert.equal(record.passed, false);
      assert.match(record.failure, /stdout|truncated|truncation|missing|zero|summary/i);
    }
  }
  const recorded = {};
  const failures = await evaluateBackendSuites(async target => ({ code: 0,
    ...capturedOutput(suiteOutput(BACKEND_SUITES[target]), "\ntest diagnostic_only ... ok\ntest result: ok. 99 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out") }),
    async (target, record) => { recorded[target] = record; });
  assert.deepEqual(failures, []);
  assert.deepEqual(Object.keys(recorded), Object.keys(BACKEND_SUITES));
  for (const [target, record] of Object.entries(recorded)) {
    assert.equal(record.passed, true);
    assert.deepEqual(record.scenarios, BACKEND_SUITES[target]);
  }
});

test("backend execution failures remain primary when machine stdout is unavailable", async () => {
  const { BACKEND_SUITES, evaluateBackendSuites } = await import("../evals/backend-runner.mjs");
  for (const [failure, expected] of [
    [{ code: 37 }, /failed/i],
    [{ code: 0, failure: "original backend timeout" }, /original backend timeout/],
    [{ code: 130, failure: "original backend cancellation", cancelled: true }, /original backend cancellation/],
    [{ code: 0, failure: "original cleanup unverified", cleanupUnverified: true }, /original cleanup unverified/],
  ]) {
    const recorded = [];
    const failures = await evaluateBackendSuites(async () => ({ output: "human diagnostics", ...failure }),
      async (_, record) => { recorded.push(record); });
    assert.equal(failures.length, failure.cancelled || failure.cleanupUnverified ? 1 : Object.keys(BACKEND_SUITES).length);
    for (const record of recorded) { assert.equal(record.passed, false); assert.match(record.failure, expected); }
  }
});
