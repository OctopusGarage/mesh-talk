#!/usr/bin/env node
// Four independently compiled, serial ASan partitions with exact union evidence.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, appendFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { command as runLogged, inventory, verifyPartition, verifyExecution, versions } from "./windows-coverage.mjs";

const partitions = [1, 2, 3, 4];
const target = "x86_64-unknown-linux-gnu";
const config = ".config/nextest-asan.toml";
const hash = value => createHash("sha256").update(value).digest("hex");
const read = file => JSON.parse(readFileSync(file, "utf8"));
const write = (file, value) => writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
const digest = value => hash(JSON.stringify(value));

export function nightlyFromManifest(text) {
  const header = text.split(/^\[/mu)[0];
  const dates = [...header.matchAll(/^date\s*=\s*"(\d{4}-\d{2}-\d{2})"\s*$/gmu)];
  assert.equal(dates.length, 1, "Installed nightly manifest date is missing or ambiguous");
  const date = dates[0][1];
  const instant = new Date(`${date}T00:00:00Z`);
  assert.ok(Number.isFinite(instant.getTime()), "Invalid nightly manifest date");
  assert.equal(instant.toISOString().slice(0, 10), date, "Invalid nightly manifest date");
  return `nightly-${date}`;
}

export function partitionArgs(mode, buildArgs, shard) {
  assert.ok(["list", "run"].includes(mode), "Invalid ASan command mode");
  assert.ok(shard === undefined || partitions.includes(shard), "Invalid ASan partition");
  return ["nextest", mode, ...buildArgs, "--config-file", path.resolve(config), "--user-config-file", "none", "--profile", "default", ...(shard ? ["--partition", `hash:${shard}/4`] : []), ...(mode === "list" ? ["--message-format", "json"] : ["--test-threads", "1", "--retries", "0", "--fail-fast", "--no-tests", "fail", "--message-format", "libtest-json-plus", "--message-format-version", "0.1"])];
}

export function verifyAsanUnion(bundles, expected) {
  assert.equal(bundles.length, 4, "Expected exactly four ASan partitions");
  assert.deepEqual(bundles.map(bundle => bundle.receipt.shard).sort(), partitions, "Duplicate or missing ASan partition");
  const full = bundles[0].full;
  for (const bundle of bundles) {
    const { receipt, selected, execution } = bundle;
    assert.equal(receipt.schema, 1, "Unsupported ASan receipt");
    assert.deepEqual(receipt.identity, expected, "ASan source/run/toolchain/config identity mismatch");
    assert.deepEqual(bundle.full, full, "ASan full inventory differs across runners");
    assert.equal(digest(bundle.full), receipt.fullSha256, "ASan full inventory digest mismatch");
    assert.equal(digest(selected), receipt.selectedSha256, "ASan selected inventory digest mismatch");
    assert.equal(hash(execution), receipt.executionSha256, "ASan execution digest mismatch");
  }
  verifyPartition(full, bundles.map(bundle => bundle.selected));
  let executedTests = 0;
  for (const { receipt, selected, execution } of bundles) {
    verifyExecution(execution, selected);
    const count = selected.filter(row => row.selected).length;
    assert.equal(receipt.executedTests, count, "ASan execution count mismatch");
    executedTests += count;
  }
  return { totalTests: full.length, executedTests, ignoredTests: full.filter(row => row.ignored).length, shards: 4 };
}

// Exported for the real stable-toolchain protocol fixture. Hosted CLI callers use
// the fixed ASan build args and verified runtime identity below, never test flags.
export async function runPartition(shard, directory, { buildArgs, identity, cargoPrefix = [], env = process.env, afterExecution = async () => {} } = {}) {
  assert.ok(partitions.includes(shard), "Invalid ASan partition");
  mkdirSync(directory, { recursive: true });
  assert.ok(!existsSync(path.join(directory, "receipt.json")), "Prior ASan receipt cannot be reused");
  const run = (mode, index, phase) => runLogged([...cargoPrefix, ...partitionArgs(mode, buildArgs, index)], { env: { ...env, RUST_TEST_THREADS: "1", NEXTEST_EXPERIMENTAL_LIBTEST_JSON: "1" }, receipt: path.join(directory, phase), progress: path.join(directory, "progress.jsonl"), phase, liveStdout: mode === "run" });
  const full = inventory(JSON.parse(await run("list", undefined, "inventory-all")));
  const selected = inventory(JSON.parse(await run("list", shard, "inventory-selected")));
  assert.ok(full.every(row => row.selected === !row.ignored), "ASan full inventory has hidden filters");
  assert.deepEqual(selected.map(({ selected: _selected, ...row }) => row), full.map(({ selected: _selected, ...row }) => row), "ASan partition inventory drift");
  assert.ok(selected.some(row => row.selected), "Empty runnable ASan partition");
  write(path.join(directory, "full.json"), full);
  write(path.join(directory, "selected.json"), selected);
  const execution = await run("run", shard, "execution");
  verifyExecution(execution, selected);
  await afterExecution();
  const receipt = { schema: 1, shard, identity, fullSha256: digest(full), selectedSha256: digest(selected), executionSha256: hash(execution), executedTests: selected.filter(row => row.selected).length };
  write(path.join(directory, "receipt.json"), receipt);
  return { receipt, full, selected, execution };
}

async function output(binary, args) { return (await runLogged(args, { binary })).trim(); }

async function cohort() {
  const rustc = await output("rustup", ["which", "--toolchain", "nightly", "rustc"]);
  const manifest = path.resolve(path.dirname(rustc), "../lib/rustlib/multirust-channel-manifest.toml");
  const toolchain = nightlyFromManifest(readFileSync(manifest, "utf8"));
  const rustcSha256 = hash(await output("rustup", ["run", "nightly", "rustc", "-vV"]));
  const cargoSha256 = hash(await output("rustup", ["run", "nightly", "cargo", "-vV"]));
  assert.ok(process.env.GITHUB_OUTPUT, "Cohort output requires GITHUB_OUTPUT");
  appendFileSync(process.env.GITHUB_OUTPUT, `toolchain=${toolchain}\nrustc=${rustcSha256}\ncargo=${cargoSha256}\n`);
  console.log(`ASan cohort: ${toolchain} (${rustcSha256})`);
}

async function expectedIdentity() {
  const { GITHUB_SHA: sourceSha, GITHUB_RUN_ID: runId, GITHUB_RUN_ATTEMPT: runAttempt, MESH_ASAN_TOOLCHAIN: toolchain, MESH_ASAN_RUSTC: rustcSha256, MESH_ASAN_CARGO: cargoSha256 } = process.env;
  assert.match(sourceSha ?? "", /^[a-f0-9]{40}$/u, "ASan requires exact workflow source SHA");
  assert.match(runId ?? "", /^[1-9]\d*$/u, "ASan requires workflow run ID");
  assert.match(runAttempt ?? "", /^[1-9]\d*$/u, "ASan requires workflow run attempt");
  assert.match(toolchain ?? "", /^nightly-\d{4}-\d{2}-\d{2}$/u, "ASan requires one dated nightly cohort");
  for (const value of [rustcSha256, cargoSha256]) assert.match(value ?? "", /^[a-f0-9]{64}$/u, "ASan requires cohort compiler fingerprints");
  assert.equal(await output("git", ["rev-parse", "HEAD"]), sourceSha, "ASan checkout differs from workflow source");
  assert.equal(await output("git", ["status", "--porcelain", "--untracked-files=no"]), "", "ASan tracked source is dirty");
  const sourceTree = await output("git", ["rev-parse", "HEAD^{tree}"]);
  for (const [key, value] of Object.entries({ RUSTFLAGS: "-Zsanitizer=address", RUSTDOCFLAGS: "-Zsanitizer=address", ASAN_OPTIONS: "detect_leaks=1", RUST_TEST_THREADS: "1" })) assert.equal(process.env[key], value, `ASan ${key} changed`);
  for (const key of ["CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"]) assert.ok(!process.env[key], `ASan ${key} may override instrumentation`);
  const inputs = ["Cargo.toml", "Cargo.lock", ".cargo/config.toml", config, "scripts/ci/asan.mjs", "scripts/ci/windows-coverage.mjs", ".github/workflows/sanitizers.yml"].map(file => [file, hash(readFileSync(file))]);
  const profileEnvironment = Object.fromEntries(Object.entries(process.env).filter(([key]) => key.startsWith("CARGO_PROFILE_")).sort(([a], [b]) => a.localeCompare(b)));
  return { sourceSha, sourceTree, runId, runAttempt, toolchain, rustcSha256, cargoSha256, nextest: versions.nextest, inputsSha256: digest(inputs), target, rustflags: process.env.RUSTFLAGS, rustdocflags: process.env.RUSTDOCFLAGS, asanOptions: process.env.ASAN_OPTIONS, testThreads: process.env.RUST_TEST_THREADS, profileEnvironment };
}

async function verifyTools(identity) {
  assert.equal(hash(await output("rustup", ["run", identity.toolchain, "rustc", "-vV"])), identity.rustcSha256, "ASan compiler differs from cohort");
  assert.equal(hash(await output("rustup", ["run", identity.toolchain, "cargo", "-vV"])), identity.cargoSha256, "ASan Cargo differs from cohort");
  assert.match(await output("cargo", [`+${identity.toolchain}`, "nextest", "--version"]), new RegExp(`^cargo-nextest ${versions.nextest.replaceAll(".", "\\.")}(?:\\s|$)`, "u"), "ASan Nextest differs from owned version");
}

// Real Linux instrumentation control, separate from the generic stable CLI test.
// The freed pointer is read through a non-inlined volatile operation so the test
// requires an actual AddressSanitizer diagnosis, not an ordinary Rust panic.
async function smoke(directory, identity) {
  await verifyTools(identity);
  mkdirSync(directory, { recursive: true });
  const temporary = mkdtempSync(path.join(os.tmpdir(), "mesh-asan-smoke-"));
  try {
    mkdirSync(path.join(temporary, "src"));
    writeFileSync(path.join(temporary, "Cargo.toml"), '[package]\nname="mesh-asan-smoke"\nversion="0.0.0"\nedition="2024"\n[workspace]\n');
    writeFileSync(path.join(temporary, "Cargo.lock"), 'version = 4\n[[package]]\nname = "mesh-asan-smoke"\nversion = "0.0.0"\n');
    writeFileSync(path.join(temporary, "src/lib.rs"), `#[test] fn clean() { let value = Box::new(37u64); assert_eq!(*value, 37); }
#[inline(never)] unsafe fn freed_read(pointer: *const u64) -> u64 { unsafe { std::ptr::read_volatile(pointer) } }
#[test] fn intentional_asan_error() { unsafe { let layout = std::alloc::Layout::new::<u64>(); let pointer = std::alloc::alloc(layout).cast::<u64>(); assert!(!pointer.is_null()); pointer.write(37); std::alloc::dealloc(pointer.cast(), layout); std::hint::black_box(freed_read(pointer)); } }
`);
    const args = ["--manifest-path", path.join(temporary, "Cargo.toml"), "--locked", "--lib", "-Z", "build-std", "--target", target, "--target-dir", path.resolve("target")];
    const run = name => runLogged([`+${identity.toolchain}`, ...partitionArgs("run", args), "-E", `test(=${name})`], { env: { ...process.env, NEXTEST_EXPERIMENTAL_LIBTEST_JSON: "1" }, receipt: path.join(directory, `smoke-${name}`), liveStdout: true });
    await run("clean");
    let failed = false;
    try { await run("intentional_asan_error"); } catch (error) { assert.match(error.message, /failed: 100\//u, "ASan smoke must fail through a real failed test"); failed = true; }
    assert.ok(failed, "Intentional use-after-free did not fail under AddressSanitizer");
    const files = Object.fromEntries(smokeFiles.map(name => [name, hash(readFileSync(path.join(directory, name)))]));
    write(path.join(directory, "smoke.json"), { schema: 1, identity, files });
    verifySmoke(directory, identity);
  } finally { rmSync(temporary, { recursive: true, force: true }); }
}

const smokeFiles = ["smoke-clean.jsonl", "smoke-clean.log", "smoke-intentional_asan_error.jsonl", "smoke-intentional_asan_error.log"];
const sanitizerMarker = /ERROR: AddressSanitizer: heap-use-after-free/u;

export function verifySmoke(directory, identity) {
  const proof = read(path.join(directory, "smoke.json"));
  assert.equal(proof.schema, 1, "Unsupported ASan smoke proof");
  assert.deepEqual(proof.identity, identity, "Stale ASan smoke identity");
  assert.deepEqual(Object.keys(proof.files ?? {}).sort(), [...smokeFiles].sort(), "ASan smoke raw channels are missing");
  const raw = Object.fromEntries(smokeFiles.map(name => {
    const bytes = readFileSync(path.join(directory, name));
    assert.equal(hash(bytes), proof.files[name], "ASan smoke channel digest mismatch");
    return [name, bytes.toString("utf8")];
  }));
  for (const [name, outcome] of [["clean", "ok"], ["intentional_asan_error", "failed"]]) {
    const stdout = raw[`smoke-${name}.jsonl`];
    const events = stdout.split(/\r?\n/u).filter(Boolean).map(JSON.parse);
    assert.deepEqual(events.map(event => [event.type, event.event]), [["suite", "started"], ["test", "started"], ["test", outcome], ["suite", outcome]], "ASan smoke must execute exactly one clean/error test without retry or truncation");
    const [started, testStart, completed, finished] = events;
    assert.equal(started.nextest?.crate, "mesh-asan-smoke", "ASan smoke package mismatch");
    assert.equal(started.nextest?.kind, "lib", "ASan smoke must use the library harness");
    assert.ok(started.nextest?.test_binary, "ASan smoke binary is missing");
    assert.deepEqual(finished.nextest, started.nextest, "ASan smoke suite identity changed");
    const eventName = `${started.nextest.crate}::${started.nextest.test_binary}$${name}`;
    assert.equal(testStart.name, eventName, "ASan smoke start identity mismatch");
    assert.equal(completed.name, eventName, "ASan smoke outcome identity mismatch");
    assert.equal(finished.passed, outcome === "ok" ? 1 : 0, "ASan smoke passing count mismatch");
    assert.equal(finished.failed, outcome === "failed" ? 1 : 0, "ASan smoke failure count mismatch");
    assert.equal(finished.ignored, 0, "ASan smoke cannot ignore its test");
    if (outcome === "ok") {
      verifyExecution(stdout, [{ selected: true, eventName }]);
      assert.doesNotMatch(stdout + raw["smoke-clean.log"], /ERROR: AddressSanitizer/u, "Clean ASan smoke contained a sanitizer error");
    } else {
      // Nextest's failed-test stdout field includes both captured test channels.
      // Require the actual failing case's diagnosis, not an unrelated stderr line.
      assert.match(completed.stdout ?? "", sanitizerMarker, "ASan smoke lacks the actual failed-case sanitizer marker");
    }
  }
}

export function artifactName(identity, shard) {
  assert.match(identity.sourceSha, /^[a-f0-9]{40}$/u, "Invalid ASan artifact source");
  for (const value of [identity.runId, identity.runAttempt]) assert.match(value, /^[1-9]\d*$/u, "Invalid ASan artifact run/attempt");
  assert.ok(partitions.includes(shard), "Invalid ASan artifact shard");
  return `asan-${identity.sourceSha}-${identity.runId}-${identity.runAttempt}-shard-${shard}`;
}

export function collectAsan(root, identity) {
  assert.deepEqual(readdirSync(root).sort(), partitions.map(shard => artifactName(identity, shard)), "Missing, stale or unexpected ASan artifacts");
  const bundles = partitions.map(shard => {
    const directory = path.join(root, artifactName(identity, shard));
    verifySmoke(directory, identity);
    const receipt = read(path.join(directory, "receipt.json"));
    assert.equal(receipt.shard, shard, "ASan receipt differs from its artifact folder shard");
    return { receipt, full: read(path.join(directory, "full.json")), selected: read(path.join(directory, "selected.json")), execution: readFileSync(path.join(directory, "execution.jsonl"), "utf8") };
  });
  return verifyAsanUnion(bundles, identity);
}

async function main([mode, first, second, ...extra]) {
  assert.equal(extra.length, 0, "Unexpected ASan arguments");
  if (mode === "cohort" && !first) return cohort();
  const identity = await expectedIdentity();
  if (mode === "smoke" && first && !second) return smoke(path.resolve(first), identity);
  if (mode === "shard" && first && second) {
    await verifyTools(identity);
    verifySmoke(second, identity);
    const result = await runPartition(Number(first), second, { buildArgs: ["--locked", "-p", "mesh-talk-core", "--lib", "-Z", "build-std", "--target", target], identity, cargoPrefix: [`+${identity.toolchain}`], afterExecution: async () => assert.deepEqual(await expectedIdentity(), identity, "ASan identity changed during execution") });
    assert.deepEqual(await expectedIdentity(), identity, "ASan identity changed during execution");
    return result;
  }
  if (mode === "verify" && first && !second) {
    assert.equal(process.env.MESH_ASAN_SHARDS_RESULT, "success", "Every actual ASan shard must succeed");
    const root = path.resolve(first);
    const summary = collectAsan(root, identity);
    console.log(`ASan full union verified: ${summary.executedTests} passed, ${summary.ignoredTests} default ignored, four exact-source partitions.`);
    return summary;
  }
  throw Error("Usage: asan.mjs cohort | smoke OUTPUT | shard INDEX OUTPUT | verify ARTIFACT_ROOT");
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2)).catch(error => { console.error(error.stack); process.exitCode = 1; });
