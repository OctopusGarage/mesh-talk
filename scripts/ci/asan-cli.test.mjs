// Requires only the pinned Nextest and an already-installed Rust toolchain.
// This verifies the partition/receipt protocol, not sanitizer instrumentation;
// the sanitizer workflow separately requires an actual Linux ASan error control.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { reservedCohorts } from "./asan.mjs";
import { versions } from "./windows-coverage.mjs";

const driver = `import { readFileSync, writeFileSync, mkdirSync, rmSync, existsSync } from "node:fs";
import { createHash } from "node:crypto";
import path from "node:path";
import { pathToFileURL } from "node:url";
const [owner, root, mode] = process.argv.slice(2);
const { runPartition, verifyAsanUnion, partitionArgs, verifySmoke, produceArchive, reuseArchive, verifyArchive } = await import(pathToFileURL(owner));
const { command: runLogged } = await import(pathToFileURL(path.join(path.dirname(owner), "windows-coverage.mjs")));
process.chdir(root);
const identity = JSON.parse(process.env.MESH_ASAN_PROTOCOL_IDENTITY);
const host = (await runLogged(["-vV"], { binary: "rustc" })).split(String.fromCharCode(10)).find(line => line.startsWith("host: ")).slice(6).trim();
const buildArgs = ["--offline", "--locked", "-p", "asan-protocol-fixture", "--lib", "--target", host];
const directory = shard => path.join(root, "evidence", String(shard));
const archiveDirectory = path.join(root, "archive");
const archiveIdentity = () => ({ ...identity, archive: JSON.parse(readFileSync(path.join(root, "build-output.json"))) });
const reuse = output => reuseArchive(archiveDirectory, path.join(root, output), archiveIdentity(), { env: { ...process.env, CARGO_NET_OFFLINE: "true" } });
if (mode === "run") {
 const archive = await produceArchive(archiveDirectory, { buildArgs, identity });
 writeFileSync(path.join(root, "build-output.json"), JSON.stringify(archive));
 rmSync(path.join(root, "target"), { recursive: true });
 const bundles = [];
 for (const shard of [1,2,3,4,5,6,7,8]) {
  const selected = await reuse(path.join("evidence", String(shard)));
  bundles.push(await runPartition(shard, directory(shard), { ...selected, identity: archiveIdentity(), env: { ...process.env, CARGO_NET_OFFLINE: "true" } }));
  if (existsSync(path.join(root, "target"))) throw Error("consumer compiled instead of reusing archive");
 }
 writeFileSync(path.join(root, "summary.json"), JSON.stringify(verifyAsanUnion(bundles, archiveIdentity())));
} else if (mode === "fail") {
 await runPartition(1, path.join(root, "failed"), { ...await reuse("failed"), identity: archiveIdentity() });
} else if (mode === "changed") {
 await runPartition(1, path.join(root, "changed"), { ...await reuse("changed"), identity: archiveIdentity(), afterExecution: async () => { throw Error("final identity changed"); } });
} else if (mode === "changed-producer") {
 await produceArchive(path.join(root, "changed-producer"), { buildArgs, identity, afterBuild: async () => { throw Error("producer identity changed"); } });
} else if (mode === "invalid-producer") {
 await produceArchive(path.join(root, process.env.MESH_ASAN_INVALID_OUTPUT), { buildArgs, identity });
} else if (mode === "wrong-source") {
 await reuseArchive(archiveDirectory, path.join(root, "wrong-source"), { ...archiveIdentity(), source: "wrong-source" });
} else if (mode === "verify-archive") {
 await verifyArchive(archiveDirectory, archiveIdentity());
} else if (mode === "missing-digests") {
 await verifyArchive(archiveDirectory, identity);
} else if (mode === "wrong-inventory") {
 const selected = await reuse("wrong-inventory");
 selected.expectedInventory = selected.expectedInventory.slice(1);
 await runPartition(1, path.join(root, "wrong-inventory"), { ...selected, identity: archiveIdentity() });
} else if (mode === "envelope") {
 const output = path.join(root, "signal-evidence"); mkdirSync(output);
 const args = ["--manifest-path", path.join(root, "signal-smoke/Cargo.toml"), "--offline", "--locked", "--lib"];
 for (const name of ["clean", "intentional_asan_error"]) {
  let failed = false;
  try { await runLogged([...partitionArgs("run", args), "-E", \`test(=\${name})\`], { env: { ...process.env, NEXTEST_EXPERIMENTAL_LIBTEST_JSON: "1", RUST_TEST_THREADS: "1" }, receipt: path.join(output, \`smoke-\${name}\`) }); }
  catch (error) { if (name !== "intentional_asan_error" || !error.message.includes("failed: 100/")) throw error; failed = true; }
  if (failed !== (name === "intentional_asan_error")) throw Error("signal protocol outcome mismatch");
 }
 const files = Object.fromEntries(["smoke-clean.jsonl", "smoke-clean.log", "smoke-intentional_asan_error.jsonl", "smoke-intentional_asan_error.log"].map(name => [name, createHash("sha256").update(readFileSync(path.join(output, name))).digest("hex")]));
 writeFileSync(path.join(output, "smoke.json"), JSON.stringify({ schema: 1, identity, files }));
 verifySmoke(output, identity);
} else if (mode === "verify") {
 const bundles = [1,2,3,4,5,6,7,8].map(shard => ({ receipt: JSON.parse(readFileSync(path.join(directory(shard), "receipt.json"))), full: JSON.parse(readFileSync(path.join(directory(shard), "full.json"))), selected: JSON.parse(readFileSync(path.join(directory(shard), "selected.json"))), execution: readFileSync(path.join(directory(shard), "execution.jsonl"), "utf8") }));
 verifyAsanUnion(bundles, archiveIdentity());
} else throw Error("unknown protocol mode");
`;

test("real pinned Nextest preserves the full default-lib union and rejects failed/altered receipts", { timeout: 120000 }, t => {
  const root = mkdtempSync(path.join(os.tmpdir(), "mesh-asan-protocol-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const directory of ["src", "tests", ".config", "scripts/ci"]) mkdirSync(path.join(root, directory), { recursive: true });
  for (const name of ["asan.mjs", "windows-coverage.mjs"]) copyFileSync(new URL(`./${name}`, import.meta.url), path.join(root, "scripts/ci", name));
  copyFileSync(new URL("../../.config/nextest-asan.toml", import.meta.url), path.join(root, ".config/nextest-asan.toml"));
  writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname="asan-protocol-fixture"\nversion="0.0.0"\nedition="2024"\n[workspace]\n');
  writeFileSync(path.join(root, "Cargo.lock"), 'version = 4\n[[package]]\nname = "asan-protocol-fixture"\nversion = "0.0.0"\n');
  const cases = Array.from({ length: 32 }, (_, index) => `#[test] fn case_${index}() { if let Some(path) = std::env::var_os("MESH_ASAN_PROTOCOL_FAIL") { match std::fs::OpenOptions::new().write(true).create_new(true).open(path) { Ok(_) => panic!("intentional protocol failure"), Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (), Err(error) => panic!("failure sentinel: {error}") } } assert_eq!(std::env::var("RUST_TEST_THREADS").unwrap(), "1"); assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some()); }`).join("\n");
  const reserved = `mod node { mod account_documents { mod tests {
    #[test] fn group_inventory_long_canonical_names_fit_payload_budget_and_preserve_required_members() {}
    mod inventory_causality { #[test] fn group_inventory_directory_known_unavailable_source_can_be_acknowledged_by_another_source() {} }
    #[test] fn group_control_unavailable_obsolete_cache_cannot_orphan_a_large_negative_head() {}
    #[test] fn group_control_normal_authorization_history_growth_keeps_bounded_current_heads() {}
  } } }`;
  writeFileSync(path.join(root, "src/lib.rs"), `${reserved}\n${cases}\n#[test] #[ignore] fn ignored() { panic!("default ignored cases must not run"); }\n`);
  // --lib must not silently expand into bin/integration targets.
  writeFileSync(path.join(root, "tests/must_not_run.rs"), '#[test] fn integration() { panic!("ASan is core lib only"); }\n');
  mkdirSync(path.join(root, "signal-smoke/src"), { recursive: true });
  writeFileSync(path.join(root, "signal-smoke/Cargo.toml"), '[package]\nname="mesh-asan-smoke"\nversion="0.0.0"\nedition="2024"\n[workspace]\n');
  writeFileSync(path.join(root, "signal-smoke/Cargo.lock"), 'version = 4\n[[package]]\nname = "mesh-asan-smoke"\nversion = "0.0.0"\n');
  // Signal/channel protocol only: this is not sanitizer instrumentation or UB.
  writeFileSync(path.join(root, "signal-smoke/src/lib.rs"), '#[test] fn clean() {}\n#[test] fn intentional_asan_error() { use std::io::Write; std::io::stderr().write_all(b"PROTOCOL FIXTURE ONLY: ERROR: AddressSanitizer: heap-use-after-free\\n").unwrap(); std::process::abort(); }\n');
  writeFileSync(path.join(root, "driver.mjs"), driver);
  const env = { ...process.env, CARGO_NET_OFFLINE: "true", CARGO_TERM_COLOR: "never", MESH_ASAN_PROTOCOL_IDENTITY: JSON.stringify({ source: "stable-protocol-fixture", platform: process.platform, architecture: process.arch, nextest: versions.nextest }) };
  for (const name of Object.keys(env)) if (name.startsWith("CARGO_LLVM_COV") || name.startsWith("__CARGO_LLVM_COV") || ["RUSTFLAGS", "RUSTDOCFLAGS", "ASAN_OPTIONS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "LLVM_PROFILE_FILE", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "MESH_VALIDATION_PROCESS_GROUP_OWNER"].includes(name)) delete env[name];
  const run = (mode, environment = env) => spawnSync(process.execPath, [path.join(root, "driver.mjs"), path.join(root, "scripts/ci/asan.mjs"), root, mode], { cwd: root, env: environment, encoding: "utf8", timeout: 90000, maxBuffer: 16 * 1024 * 1024 });
  const accepted = run("run");
  assert.equal(accepted.status, 0, `${accepted.error ?? ""}\n${accepted.stdout}\n${accepted.stderr}`);
  assert.deepEqual(JSON.parse(readFileSync(path.join(root, "summary.json"))), { totalTests: 37, executedTests: 36, ignoredTests: 1, shards: 8 });
  for (const shard of [1, 2, 3, 4, 5, 6, 7, 8]) {
    const evidence = path.join(root, "evidence", String(shard));
    const files = readdirSync(evidence, { withFileTypes: true });
    const selectedNames = JSON.parse(readFileSync(path.join(evidence, "selected.json"))).filter(row => row.selected).map(row => row.eventName.split("$")[1]);
    if (shard <= 6) assert.ok(selectedNames.every(name => !reservedCohorts.flat().includes(name)), "reserved cases must leave the hash runners");
    else assert.deepEqual(selectedNames.sort(), [...reservedCohorts[shard - 7]].sort(), "reserved runner must execute exactly its fixed cohort");
    assert.ok(files.every(entry => entry.isFile()), "execution evidence must not contain extracted runtime directories");
    for (const name of ["receipt.json", "full.json", "selected.json", "execution.jsonl"]) assert.ok(files.some(entry => entry.name === name), `missing required evidence: ${name}`);
    assert.ok(!files.some(entry => entry.name === "binaries.json"), "binary metadata belongs outside execution evidence");
  }
  for (const [mode, message] of [["wrong-source", /identity mismatch/u], ["missing-digests", /Missing trusted/u], ["wrong-inventory", /inventory differs/u]]) {
    const rejected = run(mode);
    assert.equal(rejected.status, 1, rejected.stderr);
    assert.match(rejected.stderr, message);
    assert(!existsSync(path.join(root, mode, "receipt.json")));
    assert(!existsSync(path.join(root, mode, "execution.jsonl")), "invalid archive must reject before test execution");
  }
  for (const [file, message] of [["core.tar.zst", /archive digest mismatch/u], ["archive.json", /manifest digest mismatch/u]]) {
    const filename = path.join(root, "archive", file);
    const original = readFileSync(filename);
    writeFileSync(filename, Buffer.concat([original, Buffer.from("corrupt")]));
    const rejected = run("verify-archive");
    assert.equal(rejected.status, 1);
    assert.match(rejected.stderr, message);
    writeFileSync(filename, original);
  }
  const fixtureSource = path.join(root, "src/lib.rs");
  const originalSource = readFileSync(fixtureSource, "utf8");
  const reservedFunction = reservedCohorts[0][0].split("::").at(-1);
  for (const [kind, transformed] of [
    ["renamed", originalSource.replace(reservedFunction, `${reservedFunction}_renamed`)],
    ["ignored", originalSource.replace(`#[test] fn ${reservedFunction}`, `#[test] #[ignore] fn ${reservedFunction}`)],
  ]) {
    writeFileSync(fixtureSource, transformed);
    const output = `invalid-producer-${kind}`;
    const rejected = run("invalid-producer", { ...env, MESH_ASAN_INVALID_OUTPUT: output });
    assert.equal(rejected.status, 1, rejected.stderr);
    assert.match(rejected.stderr, /ASan reserved test/u);
    assert.ok(!existsSync(path.join(root, output, "archive.json")), "invalid reserved inventory must not seal");
    writeFileSync(fixtureSource, originalSource);
  }
  const changedProducer = run("changed-producer");
  assert.equal(changedProducer.status, 1);
  assert.match(changedProducer.stderr, /producer identity changed/u);
  assert(!existsSync(path.join(root, "changed-producer/archive.json")), "changed producer cannot seal compiled bytes");
  const failed = run("fail", { ...env, MESH_ASAN_PROTOCOL_FAIL: path.join(root, "first-failure.sentinel") });
  assert.equal(failed.status, 1, `${failed.error ?? ""}\n${failed.stderr}`);
  assert.match(failed.stderr, /failed: 100/u);
  assert(!existsSync(path.join(root, "failed/receipt.json")));
  assert.ok(readdirSync(path.join(root, "failed"), { withFileTypes: true }).every(entry => entry.isFile()), "failed execution evidence must also exclude extracted runtimes");
  const selected = JSON.parse(readFileSync(path.join(root, "failed/selected.json"))).filter(row => row.selected);
  assert.ok(selected.length > 1, "the control needs later selected cases");
  const allEvents = readFileSync(path.join(root, "failed/execution.jsonl"), "utf8").trim().split(/\r?\n/u).map(JSON.parse).filter(event => event.type === "test");
  const ignored = new Set(JSON.parse(readFileSync(path.join(root, "failed/full.json"))).filter(row => row.ignored).map(row => row.eventName));
  assert.ok(allEvents.filter(event => ignored.has(event.name)).every(event => ["started", "ignored"].includes(event.event)), "default-ignored cases must remain unexecuted");
  const events = allEvents.filter(event => !ignored.has(event.name));
  const expected = selected.map(row => row.eventName).sort();
  assert.deepEqual(events.filter(event => event.event === "started").map(event => event.name).sort(), expected, "every selected case must start exactly once after an earlier failure");
  const terminal = events.filter(event => ["ok", "failed"].includes(event.event));
  assert.deepEqual(terminal.map(event => event.name).sort(), expected, "failure collection must finish the complete selected partition");
  assert.equal(terminal.filter(event => event.event === "failed").length, 1);
  assert.equal(terminal.filter(event => event.event === "ok").length, selected.length - 1);
  const firstFailure = terminal.findIndex(event => event.event === "failed");
  assert.ok(terminal.slice(firstFailure + 1).some(event => event.event === "ok"), "a real selected case must complete after the first failure");
  const changed = run("changed");
  assert.equal(changed.status, 1);
  assert.match(changed.stderr, /final identity changed/u);
  assert(!existsSync(path.join(root, "changed/receipt.json")), "changed final identity cannot certify completed tests");
  const signal = run("envelope");
  assert.equal(signal.status, 0, `${signal.error ?? ""}\n${signal.stdout}\n${signal.stderr}`);
  assert.match(readFileSync(path.join(root, "signal-evidence/smoke-intentional_asan_error.jsonl"), "utf8"), /PROTOCOL FIXTURE ONLY/u);
  const execution = path.join(root, "evidence/1/execution.jsonl");
  writeFileSync(execution, readFileSync(execution, "utf8") + " ");
  const altered = run("verify");
  assert.equal(altered.status, 1);
  assert.match(altered.stderr, /execution digest mismatch/u);
});
