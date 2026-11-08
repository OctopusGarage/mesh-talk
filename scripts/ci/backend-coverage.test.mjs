// Requires the real cargo-llvm-cov/llvm-tools installed by the owning CI job.
// Health's portable diagnostics intentionally do not require coverage tooling.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, realpathSync, renameSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { BACKEND_SUITES, LARGE_FOLDER_SCENARIO, backendCargoArgs, validateBackendOutput } from "../evals/backend-runner.mjs";

test("the real coverage CLI accumulates workspace and ignored backend execution", t => {
  const root = mkdtempSync(join(tmpdir(), "mesh-backend-coverage-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const name of ["src", "tests"]) mkdirSync(join(root, name));
  const folderCommand = workflowName => {
    const workflow = readFileSync(new URL(`../../.github/workflows/${workflowName}`, import.meta.url), "utf8");
    const lines = workflow.split("\n").map(line => line.trim());
    const commands = lines.filter(line => line.startsWith("cargo ") && line.includes("--test large_directory_enqueue "));
    assert.equal(commands.length, 1, "owning large-folder command must be unique");
    assert.ok(lines.includes("set -o pipefail"), "failed Cargo execution must survive tee");
    assert.match(commands[0], / 2> "\$RUNNER_TEMP\/backend-eval-results\/large-folder\.stderr\.log" \| tee /u, "machine evidence must exclude stderr");
    const args = commands[0].split(" 2> ")[0].split(/\s+/u).slice(1);
    assert.ok(args.includes("--locked"));
    assert.equal(args[args.indexOf("--test") + 2], LARGE_FOLDER_SCENARIO);
    assert.deepEqual(args.slice(args.indexOf("--") + 1), ["--exact", "--ignored", "--test-threads=1"]);
    return args;
  };
  const folderArgs = folderCommand("ci.yml");
  const nativeFolderArgs = folderCommand("e2e-backend.yml");
  assert.deepEqual(folderArgs.slice(0, 3), ["llvm-cov", "test", "--no-report"]);
  assert.equal(nativeFolderArgs[0], "test");
  assert.deepEqual(folderArgs.slice(3), nativeFolderArgs.slice(1));
  writeFileSync(join(root, "Cargo.toml"), '[package]\nname="mesh-talk-core"\nversion="0.1.0"\nedition="2024"\n');
  writeFileSync(join(root, "src/lib.rs"), "pub fn normal_truth() -> u64 { 1 }\npub fn ignored_truth() -> u64 { 2 }\npub fn large_folder_truth() -> u64 { 3 }\n#[test] fn normal() { assert_eq!(normal_truth(), 1); }\n");
  // A second prefix-matching case proves --exact is honored by the actual CLI.
  writeFileSync(join(root, "tests/large_directory_enqueue.rs"), `#[test] #[ignore] fn ${LARGE_FOLDER_SCENARIO}() { assert_eq!(mesh_talk_core::large_folder_truth(), 3); }\n#[test] #[ignore] fn ${LARGE_FOLDER_SCENARIO}_unrelated() { panic!("exact selection must exclude this case"); }\n`);
  writeFileSync(join(root, "tests/two_node_cli.rs"), BACKEND_SUITES.two_node_cli.map(name => `#[test] #[ignore] fn ${name}() { assert_eq!(mesh_talk_core::ignored_truth(), 2); }`).join("\n"));
  const env = { ...process.env, CARGO_TARGET_DIR: join(root, "target"), CARGO_LLVM_COV_TARGET_DIR: join(root, "coverage-target"), CARGO_TERM_COLOR: "never" };
  delete env.LLVM_PROFILE_FILE;
  const run = args => {
    const result = spawnSync("cargo", args, { cwd: root, env, encoding: "utf8", timeout: 120_000, maxBuffer: 8 * 1024 * 1024 });
    assert.equal(result.status, 0, `${args.join(" ")}\n${result.error ?? ""}\n${result.stdout}\n${result.stderr}`);
    return result.stdout;
  };
  run(["generate-lockfile", "--offline"]);
  assert.match(run(["llvm-cov", "--lib", "--locked", "--no-report"]), /1 passed; 0 failed/);
  assert.equal(existsSync(join(root, "lcov.info")), false, "unit execution must defer the only completed report");
  const output = run(backendCargoArgs("two_node_cli", true));
  validateBackendOutput(output, BACKEND_SUITES.two_node_cli);
  const folderOutput = run(folderArgs);
  assert.ok(folderOutput.split(/\r?\n/u).includes(`test ${LARGE_FOLDER_SCENARIO} ... ok`), "filtered ignored scenario did not execute");
  assert.match(folderOutput, /1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out/u);
  validateBackendOutput(run(nativeFolderArgs), [LARGE_FOLDER_SCENARIO], { allowFiltered: true });
  const logPath = join(root, "large-folder.log");
  writeFileSync(logPath, folderOutput);
  const validator = spawnSync(process.execPath, [fileURLToPath(new URL("../evals/backend-runner.mjs", import.meta.url)), "--validate-large-folder", logPath], { encoding: "utf8" });
  assert.equal(validator.status, 0, validator.stderr);
  assert.equal(existsSync(join(root, "lcov.info")), false, "both ignored passes must accumulate without an interim report");
  run(["llvm-cov", "report", "--lcov", "--output-path", "lcov.info"]);
  const coverage = readFileSync(join(root, "lcov.info"), "utf8");
  for (const line of [1, 2, 3])
    assert.match(coverage, new RegExp(`^DA:${line},[1-9]\\d*`, "m"), `source line ${line} must retain real hits`);
  assert.match(coverage, /^FNDA:[1-9]\d*,[^\n]*normal_truth/m, "earlier workspace coverage must survive the ignored pass");
  assert.match(coverage, /^FNDA:[1-9]\d*,[^\n]*ignored_truth/m, "ignored backend execution must contribute coverage");
  assert.match(coverage, /^FNDA:[1-9]\d*,[^\n]*large_folder_truth/m, "the separate ignored integration target must contribute coverage");
});

test("large-folder workflow commands preserve stdout evidence and actual Cargo failure", { skip: process.platform === "win32" }, t => {
  const root = mkdtempSync(join(tmpdir(), "mesh-folder-streams-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "bin"));
  mkdirSync(join(root, "backend-eval-results"));
  writeFileSync(join(root, "bin/cargo"), "#!/usr/bin/env bash\nprintf 'diagnostic-prefix' >&2\nprintf '%s\\n' 'test large_directory_enqueue_uses_compact_manifest_instead_of_frame_error ... ok' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'\nexit \"$MESH_FIXTURE_CARGO_EXIT\"\n", { mode: 0o755 });
  for (const workflowName of ["ci.yml", "e2e-backend.yml"]) {
    const workflow = readFileSync(new URL(`../../.github/workflows/${workflowName}`, import.meta.url), "utf8");
    const step = workflow.split(/^      - /m).find(value => value.includes("--test large_directory_enqueue "));
    const block = step.split("        run: |\n")[1].replace(/^          /gm, "");
    for (const code of [0, 37]) {
      const result = spawnSync("bash", ["-e", "-o", "pipefail", "-c", block], {
        cwd: fileURLToPath(new URL("../../", import.meta.url)), encoding: "utf8", timeout: 15000,
        env: { ...process.env, RUNNER_TEMP: root, PATH: `${join(root, "bin")}:${process.env.PATH}`, MESH_FIXTURE_CARGO_EXIT: String(code) },
      });
      assert.equal(result.status, code, `${workflowName}: ${result.stderr}`);
      assert.match(result.stderr, /diagnostic-prefix/);
      assert.doesNotMatch(readFileSync(join(root, "backend-eval-results/large-folder.log"), "utf8"), /diagnostic-prefix/);
      if (code !== 0) assert.doesNotMatch(result.stdout, /Large-folder acceptance/);
    }
  }
});

// Linux is the only hosted acceptance owner. Darwin remains an explicit local
// protocol fixture; the existing native tests in this file retain other OS work.
const skipLinuxIgnoredArchive = (platform, env) => platform === "linux" ? env.GITHUB_ACTIONS !== "true" : platform === "win32" || !!(env.CI || env.GITHUB_ACTIONS);
test("Linux ignored archive qualification runs on GitHub Linux and local Darwin only", () => {
  assert.equal(skipLinuxIgnoredArchive("linux", { CI: "1", GITHUB_ACTIONS: "true" }), false);
  assert.equal(skipLinuxIgnoredArchive("linux", {}), true);
  assert.equal(skipLinuxIgnoredArchive("linux", { CI: "1" }), true);
  assert.equal(skipLinuxIgnoredArchive("linux", { GITHUB_ACTIONS: "true" }), false, "missing hosted/image fields must reach identity validation");
  for (const env of [{}, { CI: "1" }]) assert.equal(skipLinuxIgnoredArchive("win32", env), true);
  assert.equal(skipLinuxIgnoredArchive("darwin", {}), false);
  assert.equal(skipLinuxIgnoredArchive("darwin", { CI: "1" }), true);
  assert.equal(skipLinuxIgnoredArchive("darwin", { GITHUB_ACTIONS: "true" }), true);
});

// Same official archive as default consumers; direct libtest execution supplies
// original Cargo-shaped stdout, not a synthesized Nextest report.
test("real ignored archive preserves Cargo runtime, embedded CLI and ignored-only coverage without producer outputs", { timeout: 180000, skip: skipLinuxIgnoredArchive(process.platform, process.env) }, async t => {
  const { verifyArchivedBackendEvidence } = await import("../evals/backend-runner.mjs");
  assert.equal(typeof verifyArchivedBackendEvidence, "function");
  const root = realpathSync(mkdtempSync(join(tmpdir(), "mesh-ignored-archive-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const dir of [".cargo", ".config", "empty-hooks", "host/src", "core/src", "core/tests", "integrations/src", "mcp/src"]) mkdirSync(join(root, dir), { recursive: true });
  const write = (name, value) => writeFileSync(join(root, name), value);
  write(".gitignore", "/target/\n/evidence/\n"); write(".cargo/config.toml", '[build]\njobs=1\n[env]\nRUST_TEST_THREADS="4"\n');
  write(".config/nextest.toml", 'nextest-version="0.9.150"\n[profile.default]\nretries=0\n');
  write("Cargo.toml", '[workspace]\nmembers=["host","core","integrations","mcp"]\nresolver="2"\n');
  for (const [dir, name] of [["host", "mesh-talk"], ["core", "mesh-talk-core"], ["integrations", "mesh-talk-integrations"], ["mcp", "mesh-talk-mcp"]]) {
    write(`${dir}/Cargo.toml`, `[package]\nname="${name}"\nversion="1.2.3"\nedition="2024"\nauthors=["Fixture Author"]\n${dir === "core" ? '[lib]\ncrate-type=["rlib","dylib"]\n[[bin]]\nname="mesh-talk-node"\npath="src/main.rs"\n' : ""}`);
    write(`${dir}/src/lib.rs`, '#[test] fn default_case() { assert_eq!(2+2,4); }\n');
  }
  write("core/src/lib.rs", `pub fn backend_truth() -> u64 { 17 }\npub fn bridge_truth() -> u64 { 19 }\npub fn folder_truth() -> u64 { 23 }\npub fn helper_truth() -> u64 { 29 }\n${Array.from({length:12},(_,i)=>`#[test] fn default_${i}() { assert_eq!(2+2,4); }`).join("\n")}\n`);
  write("core/src/main.rs", 'fn main() { assert!(std::env::var("LLVM_PROFILE_FILE").unwrap().contains("llvm-cov-target")); println!("{}",mesh_talk_core::helper_truth()); }\n');
  const stdoutCanary = "mesh-stdout-canary-4af9", stderrCanary = "mesh-stderr-canary-6b31";
  const body = truth => `unsafe extern "C" { fn atexit(callback: extern "C" fn()) -> i32; }
extern "C" fn publish_canary() { println!("password=${stdoutCanary}"); eprintln!("--password ${stderrCanary}"); }
assert_eq!(unsafe { atexit(publish_canary) },0);
assert_eq!(mesh_talk_core::${truth}(),${{backend_truth:17,bridge_truth:19,folder_truth:23}[truth]});
assert_eq!(std::env::current_dir().unwrap(),std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")));
assert_eq!(std::env::var("CARGO_MANIFEST_DIR").unwrap(),env!("CARGO_MANIFEST_DIR"));
assert_eq!(std::env::var("CARGO_PKG_VERSION").unwrap(),"1.2.3"); assert_eq!(std::env::var("CARGO_PKG_AUTHORS").unwrap(),"Fixture Author");
assert_eq!(std::env::var("RUST_TEST_THREADS").unwrap(),"4");
for binary in [env!("CARGO_BIN_EXE_mesh-talk-node").to_owned(),std::env::var("CARGO_BIN_EXE_mesh-talk-node").unwrap()] { let result=std::process::Command::new(binary).output().unwrap(); assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr)); assert_eq!(String::from_utf8_lossy(&result.stdout).trim(),"29"); }
assert!(std::env::var_os("MESH_IGNORED_FAIL").is_none(),"actual ignored failure");`;
  for (const [target, names] of Object.entries(BACKEND_SUITES)) write(`core/tests/${target}.rs`, names.map(name => `#[test] #[ignore] fn ${name}() {${body("backend_truth")}}`).join("\n"));
  write("core/tests/bridge_cross_lan.rs", ["bridge_first", "bridge_second", "bridge_extra_inventory_owned"].map(name => `#[test] #[ignore] fn ${name}() {${body("bridge_truth")}}`).join("\n") + '\n#[test] fn bridge_default_case() { assert_eq!(3+3,6); }\n');
  write("core/tests/large_directory_enqueue.rs", `#[test] #[ignore] fn ${LARGE_FOLDER_SCENARIO}() {${body("folder_truth")}}\n#[test] #[ignore] fn ${LARGE_FOLDER_SCENARIO}_unrelated() {panic!("exact selection required");}\n#[test] fn folder_default_case() { assert_eq!(5+5,10); }\n`);
  write("driver.mjs", `import {produce,prepareArchive,consume} from ${JSON.stringify(new URL("./windows-coverage.mjs", import.meta.url).href)};
import {runArchivedBackend,runCargo} from ${JSON.stringify(new URL("../evals/backend-runner.mjs", import.meta.url).href)};
import assert from "node:assert/strict";
import {writeFileSync,existsSync,mkdirSync,watch,readFileSync} from 'node:fs';
const [mode,directory,output,trust]=process.argv.slice(2);
const fixtureRuntime=process.platform==='linux'?undefined:{kind:'generic-archive-fixture',platform:process.platform,arch:process.arch,root:process.cwd(),qualification:'not-native-linux'};
try { if(mode==='produce') writeFileSync(output,JSON.stringify(await produce(directory,{profile:'linux-workspace',fixtureRuntime}))); else if(mode==='default') await consume(Number(process.env.MESH_DEFAULT_SHARD),directory,output,{...JSON.parse(trust),profile:'linux-workspace',fixtureRuntime}); else if(mode==='pre-aborted') {
 const controller=new AbortController(); controller.abort(new Error('fixture pre-aborted'));
 await assert.rejects(runArchivedBackend(directory,output,{...JSON.parse(trust),fixtureRuntime,signal:controller.signal}),/fixture pre-aborted/);
 assert.equal(existsSync(output),false,'pre-aborted API must not create preparation output');
 assert.equal(existsSync(process.cwd()+'/target/llvm-cov-target'),false,'pre-aborted API must not extract');
 } else if(mode==='abort-extract') {
 mkdirSync(output,{recursive:true}); const controller=new AbortController();
 const watcher=watch(output,(_event,file)=>{if(String(file)==='progress.jsonl') {const rows=readFileSync(output+'/progress.jsonl','utf8').trim().split('\\n').filter(Boolean).map(JSON.parse);if(rows.some(row=>row.phase==='extract'&&row.event==='started'))controller.abort();}});
 try { await assert.rejects(runArchivedBackend(directory,output,{...JSON.parse(trust),fixtureRuntime,signal:controller.signal}),/interrupted/); } finally {watcher.close();}
 const rows=readFileSync(output+'/progress.jsonl','utf8').trim().split('\\n').map(JSON.parse);
 assert.ok(rows.some(row=>row.phase==='extract'&&row.event==='closed'&&row.failed));
 assert.ok(!rows.some(row=>row.phase==='inventory-all'));
 assert.equal(existsSync(output+'/receipt.json'),false);assert.equal(existsSync(process.cwd()+'/target/llvm-cov-target'),false);
 } else if(mode==='loader-control') {
 const prepared=await prepareArchive(directory,output,{...JSON.parse(trust),profile:'linux-workspace',fixtureRuntime});
 const binary=Object.values(prepared.binaries['rust-binaries']).find(row=>row['binary-name']==='two_node_cli');
 const result=await runCargo('loader-control',{command:[binary['binary-path'],'--ignored','--test-threads=1'],cwd:process.cwd()+'/core',env:{...process.env,DYLD_FALLBACK_LIBRARY_PATH:'',LD_LIBRARY_PATH:''}});
 assert.ok(result.code!==0,'actual dynamic fixture must fail without loader restoration'); assert.ok(!result.cleanupUnverified); assert.match(result.stderr,/Library not loaded|shared libraries|image not found|library not found/i);
 } else await runArchivedBackend(directory,output,{...JSON.parse(trust),fixtureRuntime}); } catch(error) { console.error(error.stack); process.exitCode=1; }
`);
  const env = { ...process.env, CARGO_NET_OFFLINE: "true", CARGO_TERM_COLOR: "never", CARGO_PROFILE_DEV_DEBUG: "line-tables-only", CARGO_PROFILE_TEST_DEBUG: "line-tables-only", MESH_ARCHIVE_NODE: process.execPath };
  for (const key of Object.keys(env)) if (/^(?:__)?CARGO_LLVM_COV/u.test(key) || ["LLVM_PROFILE_FILE", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CI", "GITHUB_ACTIONS"].includes(key)) delete env[key];
  env.RUSTFLAGS = "-C prefer-dynamic";
  if (process.platform === "linux") { env.CI = process.env.CI; env.GITHUB_ACTIONS = process.env.GITHUB_ACTIONS; }
  const run = (binary, args, environment = env, status = 0) => {
    const result = spawnSync(binary, args, { cwd: root, env: environment, encoding: "utf8", timeout: 120000, maxBuffer: 32 * 1024 * 1024 });
    assert.equal(result.status, status, `${binary} ${args.join(" ")}\n${result.stdout}\n${result.stderr}\n${result.error ?? ""}`); return result;
  };
  run("cargo", ["generate-lockfile", "--offline"]); run("git", ["init", "--quiet"]); run("git", ["add", "."]);
  run("git", ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "core.hooksPath=empty-hooks", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "test: ignored archive"]);
  Object.assign(env, { GITHUB_SHA: run("git", ["rev-parse", "HEAD"]).stdout.trim(), GITHUB_RUN_ID: "1", GITHUB_RUN_ATTEMPT: "1" });
  const artifact = join(root, "target/archive"), trustFile = join(root, "target/trust.json"), target = join(root, "target/llvm-cov-target");
  const phase = (mode, output, trust = "", environment = env, status = 0) => run("bash", ["-euc", 'eval "$(cargo llvm-cov show-env --sh)"\n"$MESH_ARCHIVE_NODE" driver.mjs "$MESH_MODE" "$MESH_ARTIFACT" "$MESH_OUTPUT" "$MESH_TRUST"'], { ...environment, CARGO_TARGET_DIR: target, MESH_MODE: mode, MESH_ARTIFACT: artifact, MESH_OUTPUT: output, MESH_TRUST: trust }, status);
  phase("produce", trustFile);
  const trust = JSON.parse(readFileSync(trustFile, "utf8"));
  renameSync(target, join(root, "target/producer-output"));
  const defaultEvents = [];
  for (const shard of [1, 2]) {
    const defaultOutput = join(root, `evidence/default-${shard}`);
    const defaultResult = phase("default", defaultOutput, JSON.stringify(trust), { ...env, MESH_DEFAULT_SHARD: String(shard) });
    assert.doesNotMatch(defaultResult.stderr, /^\s*(?:Compiling|Checking) /mu);
    defaultEvents.push(...readFileSync(join(defaultOutput, "execution.jsonl"), "utf8").trim().split("\n").map(JSON.parse));
    renameSync(target, join(root, `target/default-${shard}-output`));
  }
  for (const name of ["bridge_default_case", "folder_default_case"]) assert.equal(defaultEvents.filter(event => event.type === "test" && event.event === "ok" && event.name.endsWith(`$${name}`)).length, 1, "default-only cases must execute through default partitions");
  if (process.platform !== "linux") {
    phase("consume", join(root, "evidence/ci-rejected"), JSON.stringify(trust), { ...env, CI: "1" }, 1);
    assert.ok(!existsSync(target), "generic local fixture must not qualify real CI");
  }
  phase("pre-aborted", join(root, "evidence/pre-aborted"), JSON.stringify(trust));
  phase("abort-extract", join(root, "evidence/aborted-extraction"), JSON.stringify(trust));
  phase("consume", join(root, "evidence/stale"), JSON.stringify(trust), { ...env, GITHUB_RUN_ATTEMPT: "2" }, 1);
  assert.ok(!existsSync(target));
  phase("consume", join(root, "evidence/missing-trust"), "{}", env, 1);
  assert.ok(!existsSync(target));
  phase("loader-control", join(root, "evidence/loader-control"), JSON.stringify(trust));
  renameSync(target, join(root, "target/loader-control-output"));
  const output = join(root, "evidence/ignored-results");
  const result = phase("consume", output, JSON.stringify(trust));
  assert.doesNotMatch(result.stderr, /^\s*(?:Compiling|Checking) /mu);
  const receipt = JSON.parse(readFileSync(join(output, "receipt.json"), "utf8"));
  const backendReport = JSON.parse(readFileSync(join(output, "report.json"), "utf8"));
  assert.equal(backendReport.schema, 1); assert.deepEqual(Object.keys(backendReport.suites), Object.keys(BACKEND_SUITES));
  assert.equal(Object.keys(receipt.suites).length, 6, "Bridge/folder belong in the separate ignored receipt");
  assert.equal(receipt.kind, "linux-ignored"); assert.equal(receipt.executedTests, 12, "Bridge is derived, not a duplicated four-case constant");
  assert.equal(receipt.schema, process.platform === "linux" ? 2 : "linux-archive-fixture-v1");

  const authority = { ...trust, sourceSha: env.GITHUB_SHA, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, fixtureRuntime: process.platform === "linux" ? undefined : { qualification: "not-native-linux" } };
  assert.equal(verifyArchivedBackendEvidence(artifact, output, authority).executedTests, 12);
  const originalReceipt = readFileSync(join(output,"receipt.json"),"utf8");
  for (const change of [r=>r.identity.runAttempt="2",r=>r.executedTests=0,r=>r.suites.two_node_cli.cleanupUnverified=true,r=>r.suites.two_node_cli.outputTruncated=true,r=>r.suites.two_node_cli.exitCode=37,r=>delete r.suites.bridge_cross_lan]) {
    const changed=JSON.parse(originalReceipt);change(changed);writeFileSync(join(output,"receipt.json"),JSON.stringify(changed));
    assert.throws(()=>verifyArchivedBackendEvidence(artifact,output,authority));
  }
  writeFileSync(join(output,"receipt.json"),originalReceipt);
  const stdoutFile=join(output,"two_node_cli.stdout.log"),raw=readFileSync(stdoutFile);
  writeFileSync(stdoutFile,"truncated");assert.throws(()=>verifyArchivedBackendEvidence(artifact,output,authority));writeFileSync(stdoutFile,raw);
  assert.throws(()=>verifyArchivedBackendEvidence(artifact,output,{...authority,runAttempt:"2"}));
  const coverage = readFileSync(join(output, "coverage.lcov"), "utf8");
  for (const name of ["backend_truth", "bridge_truth", "folder_truth", "helper_truth"]) assert.match(coverage, new RegExp(`^FNDA:[1-9]\\d*,[^\\n]*${name}`, "m"), `actual ignored ${name} must contribute coverage`);
  const published = readdirSync(output, { recursive: true }).map(String).filter(name => /\.(?:log|json|jsonl)$/u.test(name));
  for (const file of published) {
    const content = readFileSync(join(output, file), "utf8");
    assert.ok(!content.includes(stdoutCanary) && !content.includes(stderrCanary), `secret leaked in ${file}`);
  }
  assert.match(readFileSync(join(output, "two_node_cli.stdout.log"), "utf8"), /password=\[REDACTED\]/u);
  assert.match(readFileSync(join(output, "two_node_cli.stderr.log"), "utf8"), /--password \[REDACTED\]/u);
  const files = readdirSync(output, { recursive: true }).map(String); assert.ok(files.every(name => /\.(?:json|jsonl|log|lcov)$/u.test(name)), "runtime binaries must remain outside evidence");
  renameSync(target, join(root, "target/success-output"));
  const failed = join(root, "evidence/failed-results");
  phase("consume", failed, JSON.stringify(trust), { ...env, MESH_IGNORED_FAIL: "1" }, 1);
  assert.ok(!existsSync(join(failed, "receipt.json"))); const report = JSON.parse(readFileSync(join(failed, "report.json"), "utf8"));
  assert.deepEqual(Object.keys(report.suites), Object.keys(BACKEND_SUITES), "ordinary failures retain independent backend admission but reject later phases");
  assert.ok(Object.values(report.suites).every(row => !row.passed));
  t.diagnostic(process.platform === "linux" ? "Actual native Linux ignored archive/loader coverage qualified; full product qualification remains" : "Darwin archive/loader protocol only; Linux ABI/full product evidence remains unqualified");
});
