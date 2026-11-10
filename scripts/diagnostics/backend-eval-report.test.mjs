import { test } from "node:test";
import assert from "node:assert/strict";
import { toolVersionFixture } from "./test-fixtures/tool-versions.mjs";

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
  const { env } = toolVersionFixture(directory, { ...process.env, CI: "1", MESH_VALIDATION_PROCESS_GROUP_OWNER: "", NODE_TEST_CONTEXT: undefined });
  const result = spawnSync(process.execPath, [fileURLToPath(new URL("../validation/stage.mjs", import.meta.url)), "fault-fixtures", "--", process.execPath, "--test", "--test-reporter=tap", "--test-name-pattern", "backend (output cap|timeout)|bounded child execution|browser wrapper exposes", ...files], { cwd: directory, env, encoding: "utf8", timeout: 25000 });
  assert.equal(result.status, 0, result.stdout + result.stderr + JSON.stringify({ error: result.error?.message, signal: result.signal }));
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

// The archive adapter consumes actual libtest text through the existing owner.
// These controls exercise selection and evidence independently of real LLVM tools.
async function archivedFixture(t) {
  const { mkdtempSync, realpathSync, mkdirSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const path = await import('node:path');
  const { BACKEND_SUITES, LARGE_FOLDER_SCENARIO } = await import('../evals/backend-runner.mjs');
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'backend-archive-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const target = path.join(root, 'target/llvm-cov-target');
  const cwd = path.join(root, 'core');
  const rustLibDir = path.join(root, 'rust/lib');
  for (const directory of [path.join(target,'debug/deps'),cwd,rustLibDir]) mkdirSync(directory,{recursive:true});
  const helper = path.join(target,'debug/mesh-talk-node'); writeFileSync(helper,'helper',{mode:0o755});
  const suites = {...BACKEND_SUITES,bridge_cross_lan:['bridge_a','bridge_b'],large_directory_enqueue:[LARGE_FOLDER_SCENARIO,'unrelated_folder']};
  const listing = {'rust-suites':{},'test-count':0}; const binaries = {'rust-build-meta':{'target-directory':target,'linked-paths':[],'non-test-binaries':{core:[{name:'mesh-talk-node',kind:'bin-exe',path:'debug/mesh-talk-node'}]}},'rust-binaries':{}};
  const full = [];
  for(const [name,names] of Object.entries(suites)) {
    const id=`core::${name}`, binaryPath=path.join(target,'debug/deps',name); writeFileSync(binaryPath,'libtest',{mode:0o755});
    listing['rust-suites'][id]={'binary-id':id,'package-id':'core','package-name':'mesh-talk-core','binary-name':name,kind:'test',status:'listed',testcases:Object.fromEntries(names.map(n=>[n,{kind:'test',ignored:true,'filter-match':{status:'mismatch',reason:'ignored'}}]))};
    binaries['rust-binaries'][id]={'binary-id':id,'package-id':'core','binary-name':name,kind:'test','binary-path':binaryPath};
    for(const n of names) full.push({id:`${id}::${n}`,eventName:`mesh-talk-core::${name}$${n}`,ignored:true,selected:false});
    listing['test-count']+=names.length;
  }
  for (const name of ['bridge_cross_lan','large_directory_enqueue']) {
    const id=`core::${name}`, test='default_case';
    listing['rust-suites'][id].testcases[test]={kind:'test',ignored:false,'filter-match':{status:'matches'}};
    listing['test-count']++;
    full.push({id:`${id}::${test}`,eventName:`mesh-talk-core::${name}$${test}`,ignored:false,selected:true});
  }
  const env={LLVM_PROFILE_FILE:path.join(target,'profiles/%m-%p.profraw')};
  const prepared={manifest:{identity:{root,targetDirectory:target,instrumentationEnvironment:env}},listings:[listing],observed:[full],binaries,metadata:{packages:[{id:'core',name:'mesh-talk-core',version:'1.2.3',authors:['Author'],manifest_path:path.join(cwd,'Cargo.toml'),targets:Object.keys(suites).map(name=>({name,kind:['test']}))}]}};
  return {prepared,env,rustLibDir,root,target,cwd,helper};
}

test('archived ignored plan resolves original executables, complete bridge inventory and exact folder selection before admission', async t=>{
  const {archivedBackendPlan,BACKEND_SUITES,LARGE_FOLDER_SCENARIO}=await import('../evals/backend-runner.mjs');
  const fixture=await archivedFixture(t);
  const plan=archivedBackendPlan(fixture.prepared,fixture);
  assert.deepEqual(plan.map(s=>s.target),[...Object.keys(BACKEND_SUITES),'bridge_cross_lan','large_directory_enqueue']);
  assert.deepEqual(plan[4].expected,['bridge_a','bridge_b']);
  assert.equal(plan[4].allowFiltered,true);
  assert.equal(plan[5].allowFiltered,true);
  assert.deepEqual(plan[5].expected,[LARGE_FOLDER_SCENARIO]);
  assert.deepEqual(plan[5].args,[LARGE_FOLDER_SCENARIO,'--exact','--ignored','--test-threads=1']);
  for(const item of plan){assert.equal(item.cwd,fixture.cwd);assert.equal(item.env.CARGO_MANIFEST_DIR,fixture.cwd);assert.equal(item.env.CARGO_PKG_VERSION,'1.2.3');assert.equal(item.env['CARGO_BIN_EXE_mesh-talk-node'],fixture.helper);assert.equal(item.env.RUST_TEST_THREADS,'4');assert.equal(item.env.LLVM_PROFILE_FILE,fixture.env.LLVM_PROFILE_FILE);}
});

test('archived ignored plan fails closed for missing, foreign, duplicate and changed ignored inventories',async t=>{
  const {archivedBackendPlan}=await import('../evals/backend-runner.mjs');
  assert.equal(typeof archivedBackendPlan,'function');
  const fixture=await archivedFixture(t);
  for(const change of [
    p=>delete p.binaries['rust-binaries']['core::two_node_cli'],
    p=>p.binaries['rust-binaries'].duplicate=p.binaries['rust-binaries']['core::two_node_cli'],
    p=>p.binaries['rust-binaries']['core::two_node_cli']['binary-path']='/foreign/test',
    p=>p.listings[0]['rust-suites']['core::two_node_cli'].testcases.extra={ignored:true},
    p=>p.listings[0]['rust-suites']['core::bridge_cross_lan'].testcases={},
    p=>p.listings[0]['rust-suites']['core::two_node_cli'].testcases.two_cli_nodes_exchange_a_dm.ignored=false,
  ]){const prepared=structuredClone(fixture.prepared);change(prepared);assert.throws(()=>archivedBackendPlan(prepared,fixture));}
  assert.throws(()=>archivedBackendPlan(fixture.prepared,{...fixture,env:{LLVM_PROFILE_FILE:'/foreign/profiles'}}));
});

test('backend command forwards package cwd without changing stdout evidence',async t=>{
  const {runCargo}=await import('../evals/backend-runner.mjs');
  const fixture=await archivedFixture(t);
  const result=await runCargo('two_node_cli',{command:[process.execPath,'-e','console.log(process.cwd())'],cwd:fixture.cwd});
  assert.equal(result.code,0);assert.equal(result.stdout.trim(),fixture.cwd);assert.equal(result.outputTruncated,false);
});

test('archived backend retains raw stdout validation, failure precedence and original phase admission',async t=>{
  const {archivedBackendPlan,evaluateArchivedBackend,BACKEND_SUITES}=await import('../evals/backend-runner.mjs');
  const fixture=await archivedFixture(t),plan=archivedBackendPlan(fixture.prepared,fixture);
  const expected=Object.fromEntries(plan.map(item=>[item.target,item.expected]));
  const good=target=>({code:0,...capturedOutput(suiteOutput(expected[target]))});
  const attempted=[];
  const success=await evaluateArchivedBackend(plan,async(target,options)=>{attempted.push(target);assert.equal(options.cwd,fixture.cwd);assert.deepEqual(options.command,[plan.find(row=>row.target===target).binary,...plan.find(row=>row.target===target).args]);return good(target);});
  assert.deepEqual(success.failures,[]);assert.deepEqual(attempted,plan.map(item=>item.target));
  for(const variant of [
    target=>({...good(target),stdout:''}),
    target=>({...good(target),outputTruncated:true}),
    target=>({...good(target),stdout:suiteOutput([])}),
    target=>({...good(target),code:37,stdout:undefined}),
    target=>({...good(target),failure:'original timeout',stdout:undefined}),
    target=>({...good(target),cancelled:true,stdout:undefined}),
    target=>({...good(target),cleanupUnverified:true,stdout:undefined}),
  ]) {
    const tried=[];const result=await evaluateArchivedBackend(plan,async target=>{tried.push(target);return variant(target);});
    assert.ok(result.failures.length);assert.ok(result.failures.every(failure=>!/TypeError/u.test(failure)));
    const stop=variant('two_node_cli');assert.deepEqual(tried,stop.cancelled||stop.cleanupUnverified?['two_node_cli']:Object.keys(BACKEND_SUITES));
    if(stop.code===37) assert.match(result.failures[0],/Backend suite failed/u);
    if(stop.failure) assert.match(result.failures[0],/original timeout/u);
  }
});

test('pre-aborted ignored archive API admits no preparation output or authority probes',async t=>{
  const {runArchivedBackend}=await import('../evals/backend-runner.mjs');
  const {mkdtempSync,existsSync,rmSync}=await import('node:fs');const {tmpdir}=await import('node:os');const {join}=await import('node:path');
  const root=mkdtempSync(join(tmpdir(),'backend-pre-aborted-'));t.after(()=>rmSync(root,{recursive:true,force:true}));
  const controller=new AbortController(),reason=new Error('pre-aborted before archive admission');controller.abort(reason);
  const output=join(root,'unadmitted');
  await assert.rejects(runArchivedBackend(join(root,'no-archive'),output,{signal:controller.signal}),error=>error===reason);
  assert.equal(existsSync(output),false);
});
