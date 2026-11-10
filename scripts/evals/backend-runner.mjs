import assert from "node:assert/strict";
import { readFileSync, realpathSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { prepareArchive, verifyPreparedArchive, validateLcov, collectDefaultArchive, verifyIdentity } from "../ci/windows-coverage.mjs";
import { execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile, rm } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { createHash } from "node:crypto";

import { runCommand, requireCommandStdout } from "../pr-platform/guest.mjs";

export const BACKEND_SUITES = {
  two_node_cli: ["two_cli_nodes_discover_by_routed_query_and_exchange_a_dm", "two_cli_nodes_exchange_a_dm", "participant_cli_reopens_same_profile_on_requested_port_after_quit_eof_and_input_error", "participant_cli_probe_respects_persisted_invisible_mode"],
  persistent_history: ["message_history_survives_restart"],
  post_office_offline: ["offline_dm_delivered_via_post_office"],
  channel_and_file_cli: ["two_cli_nodes_exchange_a_channel_message", "two_cli_nodes_transfer_a_file"],
};

export const LARGE_FOLDER_SCENARIO = "large_directory_enqueue_uses_compact_manifest_instead_of_frame_error";

export function validateBackendOutput(output, expected, { allowFiltered = false } = {}) {
  const successful = [...output.matchAll(/^test (\S+) \.\.\. ok\s*$/gm)].map(match => match[1]);
  assert.equal(successful.length, expected.length, "Missing, duplicate or zero executed backend scenarios");
  for (const name of expected) assert.equal(successful.filter(value => value === name).length, 1, `Missing successful scenario: ${name}`);
  const summaries = [...output.matchAll(/test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out/g)];
  assert.equal(summaries.length, 1, "Missing unique Cargo test summary");
  assert.equal(Number(summaries[0][1]), expected.length, "Incorrect scenario count");
  for (const count of summaries[0].slice(2, allowFiltered ? 5 : 6)) assert.equal(Number(count), 0, "Failed, ignored, measured or filtered backend test");
  return successful;
}

export function backendCargoArgs(target, coverage = false) {
  assert.ok(Object.hasOwn(BACKEND_SUITES, target), `Unknown backend suite: ${target}`);
  return [...(coverage ? ["llvm-cov", "--no-report"] : ["test"]), "--locked", "-p", "mesh-talk-core", "--test", target, "--", "--ignored", "--test-threads=1", "--nocapture"];
}

export async function runCargo(target, { env = process.env, command = ["cargo", ...backendCargoArgs(target, env.EVAL_COVERAGE === "1")], maxOutputBytes = 16 * 1024 * 1024, timeoutMs = 20 * 60 * 1000, signal, cwd } = {}) {
  const result = await runCommand(command[0], command.slice(1), {
    cwd, env: { ...env, CARGO_TERM_COLOR: "never" }, timeoutMs, maxOutputBytes, outputLimitIsFailure: true, signal,
  });
  return { ...result, failure: result.timedOut ? `Backend suite timeout: ${target}` : result.cancelled ? "Backend evaluation cancelled" : result.error };
}

export async function evaluateBackendSuites(runSuite, recordSuite, { suites = BACKEND_SUITES, allowFiltered = false } = {}) {
  const failures = [];
  for (const [target, expected] of Object.entries(suites)) {
    const started = Date.now();
    const record = { passed: false };
    try {
      const result = await runSuite(target);
      Object.assign(record, result);
      assert.ok(!result.failure, result.failure);
      assert.ok(!result.cancelled, "Backend evaluation cancelled");
      assert.ok(!result.cleanupUnverified, "Backend cleanup unverified");
      assert.equal(result.code, 0, `Backend suite failed: ${target}`);
      record.scenarios = validateBackendOutput(requireCommandStdout(result), expected, { allowFiltered });
      record.passed = true;
    } catch (error) {
      record.failure = String(error.message);
      failures.push(`${target}: ${record.failure}`);
    }
    record.elapsedMs = Date.now() - started;
    // Independent fixtures must all run, but no later pass can erase a failure.
    // Failure to preserve evidence remains fatal rather than an incomplete pass.
    await recordSuite(target, record);
    if (record.cancelled || record.cleanupUnverified) break;
  }
  return failures;
}

const sha256 = value => createHash("sha256").update(value).digest("hex");
const executableHash = file => sha256(readFileSync(file));
const redact = text => text.replace(/(--password\s+|password[=:]\s*)\S+/gi, "$1[REDACTED]");

function ignoredSelection(full) {
  return [...Object.entries(BACKEND_SUITES), ["bridge_cross_lan", null], ["large_directory_enqueue", [LARGE_FOLDER_SCENARIO]]].map(([target, names]) => {
    const prefix = `mesh-talk-core::${target}$`;
    const targetRows = full.filter(row => row.eventName.startsWith(prefix));
    if (Object.hasOwn(BACKEND_SUITES, target)) assert.ok(targetRows.every(row => row.ignored && !row.selected), `Changed backend inventory: ${target}`);
    const rows = targetRows.filter(row => row.ignored);
    assert.ok(rows.length && rows.every(row => !row.selected), `Missing or changed ignored inventory: ${target}`);
    const all = rows.map(row => row.eventName.slice(prefix.length)).sort();
    assert.equal(new Set(all).size, all.length, `Duplicate ignored inventory: ${target}`);
    if (names && target !== "large_directory_enqueue") assert.deepEqual(all, [...names].sort(), `Ignored backend selection changed: ${target}`);
    names ??= all;
    const selected = names.map(name => {
      const matching = rows.filter(row => row.eventName === `${prefix}${name}`);
      assert.equal(matching.length, 1, `Missing ignored scenario: ${name}`);
      return matching[0].id;
    });
    return { target, expected: names, ids: selected, allowFiltered: !Object.hasOwn(BACKEND_SUITES, target) };
  });
}

// Resolve the complete ignored contract before admitting any application process.
// These are libtest commands: their original channels feed the existing validator.
export function archivedBackendPlan(prepared, { env = process.env, rustLibDir } = {}) {
  const { manifest, binaries, metadata, listings } = prepared;
  const root = manifest.identity.root, target = manifest.identity.targetDirectory;
  assert.equal(target, join(root, "target/llvm-cov-target"), "Ignored archive requires the original Linux coverage target");
  assert.equal(realpathSync(target), target, "Ignored target may not redirect");
  const inside = file => {
    const relative = path.relative(target, file);
    assert.ok(relative && !relative.startsWith("..") && !path.isAbsolute(relative), "Ignored executable escaped archive target");
    assert.equal(realpathSync(file), file, "Ignored executable may not redirect");
    assert.ok(statSync(file).isFile() && (process.platform === "win32" || statSync(file).mode & 0o111), "Ignored executable is missing or not executable");
    return file;
  };
  assert.ok(path.isAbsolute(rustLibDir ?? "") && statSync(rustLibDir).isDirectory(), "Actual Rust library directory required");
  for (const [key, value] of Object.entries(manifest.identity.instrumentationEnvironment)) assert.equal(env[key], value, `Ignored instrumentation environment changed: ${key}`);
  assert.ok(path.isAbsolute(env.LLVM_PROFILE_FILE ?? "") && env.LLVM_PROFILE_FILE.startsWith(`${target}${path.sep}`), "Ignored LLVM profile must remain in coverage target");
  const packages = metadata.packages.filter(pkg => pkg.name === "mesh-talk-core");
  assert.equal(packages.length, 1, "Ignored archive requires unique core package");
  const pkg = packages[0], cwd = path.dirname(pkg.manifest_path);
  assert.ok(path.relative(root, cwd) && !path.relative(root, cwd).startsWith(".."), "Core package cwd escaped checkout");
  assert.equal(realpathSync(cwd), cwd, "Core package cwd may not redirect");
  const nonTests = binaries["rust-build-meta"]["non-test-binaries"]?.[pkg.id] ?? [];
  const helpers = nonTests.filter(row => row.kind === "bin-exe" && row.name === "mesh-talk-node");
  assert.equal(helpers.length, 1, "Original embedded mesh-talk-node must exist uniquely");
  const helper = inside(resolve(target, helpers[0].path));
  assert.equal(helper, join(target, "debug/mesh-talk-node"), "Embedded CLI must remain at its original compile-time path");
  const linked = binaries["rust-build-meta"]["linked-paths"] ?? [];
  assert.ok(Array.isArray(linked), "Unsupported archive linked library metadata");
  const loaderPaths = linked.map(value => {
    assert.equal(typeof value, "string", "Unsupported linked library path");
    const file = resolve(target, value);
    const relative = path.relative(target, file);
    assert.ok(!relative.startsWith("..") && !path.isAbsolute(relative), "Linked library escaped archive target");
    assert.ok(statSync(file).isDirectory(), "Archived linked library directory missing");
    return file;
  });
  const loader = process.platform === "darwin" ? "DYLD_FALLBACK_LIBRARY_PATH" : "LD_LIBRARY_PATH";
  const version = /^(\d+)\.(\d+)\.(\d+)(?:-(.+))?$/u.exec(pkg.version);
  assert.ok(version, "Invalid Cargo package version");
  const runtimeEnv = { ...env, CARGO_TERM_COLOR: "never", CARGO_NET_OFFLINE: "true", RUST_TEST_THREADS: "4", CARGO_MANIFEST_DIR: cwd,
    CARGO_MANIFEST_PATH: pkg.manifest_path, CARGO_PKG_NAME: pkg.name, CARGO_PKG_VERSION: pkg.version,
    CARGO_PKG_VERSION_MAJOR: version[1], CARGO_PKG_VERSION_MINOR: version[2], CARGO_PKG_VERSION_PATCH: version[3], CARGO_PKG_VERSION_PRE: version[4] ?? "",
    CARGO_PKG_AUTHORS: (pkg.authors ?? []).join(":"), CARGO_PKG_DESCRIPTION: pkg.description ?? "", CARGO_PKG_HOMEPAGE: pkg.homepage ?? "", CARGO_PKG_REPOSITORY: pkg.repository ?? "", CARGO_PKG_LICENSE: pkg.license ?? "", CARGO_PKG_LICENSE_FILE: pkg.license_file ?? "", CARGO_PKG_RUST_VERSION: pkg.rust_version ?? "", CARGO_PKG_README: pkg.readme ?? "",
    "CARGO_BIN_EXE_mesh-talk-node": helper,
    [loader]: [...loaderPaths, join(target, "debug/deps"), join(target, "debug"), rustLibDir, ...(env[loader] ? env[loader].split(path.delimiter) : [])].join(path.delimiter),
  };
  const helperSha256 = executableHash(helper);
  const entries = ignoredSelection(prepared.observed[0]);
  return entries.map(({ target: name, expected, ids, allowFiltered }) => {
    const suites = Object.values(listings[0]["rust-suites"]).filter(row => row["package-id"] === pkg.id && row.kind === "test" && row["binary-name"] === name);
    assert.equal(suites.length, 1, `Missing or duplicate ignored suite: ${name}`);
    const suite = suites[0];
    const rows = Object.entries(suite.testcases);
    const ignored = rows.filter(([, row]) => row.ignored).map(([test]) => test).sort();
    assert.ok(ignored.length, `Empty ignored suite: ${name}`);
    if (Object.hasOwn(BACKEND_SUITES, name)) assert.equal(rows.length, ignored.length, `Unexpected non-ignored case in ${name}`);
    if (expected && name !== "large_directory_enqueue") assert.deepEqual(ignored, [...expected].sort(), `Ignored backend selection changed: ${name}`);
    for (const test of expected) assert.equal(rows.filter(([key, row]) => key === test && row.ignored).length, 1, `Missing ignored scenario: ${test}`);
    const found = Object.values(binaries["rust-binaries"]).filter(row => row["package-id"] === pkg.id && row.kind === "test" && row["binary-name"] === name);
    assert.equal(found.length, 1, `Missing or duplicate ignored executable: ${name}`);
    assert.equal(found[0]["binary-id"], suite["binary-id"], "Ignored suite and executable identity differ");
    assert.equal(pkg.targets.filter(row => row.name === name && row.kind.includes("test")).length, 1, "Ignored executable differs from Cargo target");
    const binary = inside(found[0]["binary-path"]);
    assert.equal(path.dirname(binary), join(target, "debug/deps"), "Ignored harness must retain original target layout");
    return { target: name, expected, ids, binary, binarySha256: executableHash(binary), helperSha256, cwd, env: runtimeEnv,
      args: name === "large_directory_enqueue" ? [LARGE_FOLDER_SCENARIO, "--exact", "--ignored", "--test-threads=1"] : ["--ignored", "--test-threads=1", ...(Object.hasOwn(BACKEND_SUITES, name) ? ["--nocapture"] : [])],
      allowFiltered };
  });
}

export async function evaluateArchivedBackend(plan, run = runCargo, recordSuite = async () => {}) {
  const records = {}, failures = [];
  const runSuite = target => {
    const item = plan.find(row => row.target === target);
    return run(target, { command: [item.binary, ...item.args], cwd: item.cwd, env: item.env });
  };
  const record = async (target, result) => {
    records[target] = result;
    await recordSuite(plan.find(row => row.target === target), result);
  };
  failures.push(...await evaluateBackendSuites(runSuite, record));
  // Preserve the old shell's phase admission after the independent backend set.
  if (!failures.length) for (const item of plan.filter(row => !Object.hasOwn(BACKEND_SUITES, row.target))) {
    failures.push(...await evaluateBackendSuites(runSuite, record, { suites: { [item.target]: item.expected }, allowFiltered: item.allowFiltered }));
    if (failures.length) break;
  }
  return { records, failures };
}

// Pure collection consumes sealed inventory and original channels. Its caller
// supplies trusted current source/run/attempt and producer hashes, not artifact claims.
export function verifyArchivedBackendEvidence(directory, output, trusted) {
  for (const value of [trusted.manifestSha256, trusted.archiveSha256]) assert.match(value ?? "", /^[a-f0-9]{64}$/u, "Trusted ignored archive digests required");
  const manifestBytes = readFileSync(join(directory, "manifest.json"));
  assert.equal(sha256(manifestBytes), trusted.manifestSha256, "Trusted ignored manifest digest mismatch");
  const manifest = JSON.parse(manifestBytes);
  assert.equal(manifest.identity.archiveProfile, "linux-workspace", "Ignored evidence requires Linux archive profile");
  assert.equal(manifest.schema, trusted.fixtureRuntime ? "linux-archive-fixture-v1" : 2, "Unsupported ignored archive schema");
  if (trusted.fixtureRuntime) assert.ok(!process.env.CI && !process.env.GITHUB_ACTIONS, "Generic ignored fixture cannot qualify CI");
  for (const [key, pattern] of [["sourceSha", /^[a-f0-9]{40}$/u], ["runId", /^[1-9]\d*$/u], ["runAttempt", /^[1-9]\d*$/u]]) {
    assert.match(trusted[key] ?? "", pattern, `Trusted ignored ${key} required`);
    assert.equal(manifest.identity[key], trusted[key], `Ignored ${key} mismatch`);
  }
  assert.equal(manifest.archiveSha256, trusted.archiveSha256, "Ignored archive digest mismatch");
  const fullBytes = readFileSync(join(directory, "inventory-0.json"));
  assert.equal(sha256(fullBytes), manifest.inventoryFiles["inventory-0.json"], "Ignored source inventory digest mismatch");
  const selection = ignoredSelection(JSON.parse(fullBytes));
  const receipt = JSON.parse(readFileSync(join(output, "receipt.json")));
  assert.equal(receipt.schema, manifest.schema); assert.equal(receipt.kind, "linux-ignored");
  assert.equal(receipt.channelRedaction, "recognizable-passwords-v1", "Ignored channel redaction contract missing");
  verifyIdentity(manifest.identity, receipt.identity);
  for (const key of ["archiveSha256", "manifestSha256"]) assert.equal(receipt[key], trusted[key]);
  assert.equal(receipt.adapterSha256, executableHash(fileURLToPath(import.meta.url)), "Ignored adapter source changed");
  assert.equal(receipt.featureResolutionSha256, manifest.featureResolutionSha256);
  assert.deepEqual(receipt.inventoryFiles, manifest.inventoryFiles);
  const inventoryBytes = readFileSync(join(output, "ignored-inventory.json"));
  assert.equal(sha256(inventoryBytes), receipt.selectedInventorySha256, "Ignored selected inventory digest mismatch");
  const inventory = JSON.parse(inventoryBytes);
  assert.deepEqual(inventory.map(({ target, expected, ids, allowFiltered }) => ({ target, expected, ids, allowFiltered })), selection, "Ignored selected scenarios differ from sealed contract");
  for (const item of inventory) {
    for (const value of [item.binarySha256, item.helperSha256]) assert.match(value ?? "", /^[a-f0-9]{64}$/u, "Ignored executable fingerprints required");
    assert.equal(path.dirname(item.binary), join(manifest.identity.targetDirectory, "debug/deps"), "Ignored executable path changed");
    const relative = path.relative(manifest.identity.root, item.cwd);
    assert.ok(relative && !relative.startsWith("..") && !path.isAbsolute(relative), "Ignored cwd escaped source");
    const args = item.target === "large_directory_enqueue" ? [LARGE_FOLDER_SCENARIO, "--exact", "--ignored", "--test-threads=1"] : ["--ignored", "--test-threads=1", ...(Object.hasOwn(BACKEND_SUITES, item.target) ? ["--nocapture"] : [])];
    assert.deepEqual(item.args, args, "Ignored libtest selector changed");
  }
  assert.deepEqual(Object.keys(receipt.suites), selection.map(item => item.target), "Ignored execution incomplete");
  for (const item of selection) {
    const result = receipt.suites[item.target];
    assert.ok(result.passed && !result.failure && !result.cancelled && !result.cleanupUnverified, "Ignored suite did not complete successfully");
    assert.equal(result.exitCode, 0); assert.equal(result.signal, null); assert.equal(result.outputTruncated, false);
    const stem = item.target === "large_directory_enqueue" ? "large-folder" : item.target;
    const stdout = readFileSync(join(output, `${stem}.stdout.log`), "utf8"), stderr = readFileSync(join(output, `${stem}.stderr.log`), "utf8");
    assert.equal(sha256(stdout), result.stdoutSha256, "Ignored stdout digest mismatch"); assert.equal(sha256(stderr), result.stderrSha256, "Ignored stderr digest mismatch");
    assert.deepEqual(validateBackendOutput(stdout, item.expected, { allowFiltered: item.allowFiltered }), result.scenarios);
    assert.equal(sha256(readFileSync(join(output, `${stem}.log`))), result.evidenceDigest, "Ignored diagnostic digest mismatch");
  }
  assert.equal(receipt.executedTests, selection.reduce((sum, item) => sum + item.expected.length, 0));
  const coverage = readFileSync(join(output, "coverage.lcov"));
  assert.equal(sha256(coverage), receipt.coverageSha256); assert.deepEqual(validateLcov(coverage.toString("utf8")), receipt.coverage);
  return receipt;
}

// Collect default and ignored cohorts through their existing strict owners.
export async function collectLinuxWorkspace(
  directory,
  outputs,
  ignoredOutput,
  trusted,
) {
  const defaults = await collectDefaultArchive(directory, outputs, trusted, {
    profile: "linux-workspace",
  });
  const ignored = verifyArchivedBackendEvidence(
    directory,
    ignoredOutput,
    trusted,
  );
  const report = JSON.parse(
    readFileSync(join(ignoredOutput, "report.json"), "utf8"),
  );
  assert.equal(report.schema, 1);
  assert.equal(report.sourceSha, trusted.sourceSha);
  assert.equal(
    report.platform,
    trusted.fixtureRuntime ? ignored.identity.platform : "linux",
  );
  assert.equal(report.native, !trusted.fixtureRuntime);
  assert.equal(report.mocked, false);
  assert.ok(!report.failure && !report.failures, "Backend report failed");
  assert.deepEqual(
    Object.keys(report.suites),
    Object.keys(BACKEND_SUITES),
    "Backend report lost its original four suites",
  );
  for (const target of Object.keys(BACKEND_SUITES)) {
    const {
      stdoutSha256: _stdout,
      stderrSha256: _stderr,
      outputTruncated: _truncated,
      ...expected
    } = ignored.suites[target];
    assert.deepEqual(
      report.suites[target],
      expected,
      "Backend report differs from verified ignored execution",
    );
  }
  return { ...defaults, ignoredTests: ignored.executedTests };
}
export async function runArchivedBackend(directory, output, options = {}) {
  const prepared = await prepareArchive(directory, output, { ...options, profile: "linux-workspace" });
  const executionSuites = {};
  const report = { schema: 1, sourceSha: prepared.manifest.identity.sourceSha, platform: process.platform, native: !options.fixtureRuntime, mocked: false, suites: {}, startedAt: new Date().toISOString(), kdf: "fast-test-kdf via existing self-dev-dependency; release binary unaffected" };
  const rust = await runCargo("runtime-rust-libdir", { command: [process.env.RUSTC || "rustc", "--print", "target-libdir"], signal: options.signal });
  assert.ok(!rust.failure && rust.code === 0 && !rust.cleanupUnverified, rust.failure || "Cannot resolve actual Rust library directory");
  const plan = archivedBackendPlan(prepared, { rustLibDir: requireCommandStdout(rust).trim() });
  await writeFile(join(output, "ignored-inventory.json"), JSON.stringify(plan.map(({ target, expected, ids, binary, binarySha256, helperSha256, cwd, args, allowFiltered }) => ({ target, expected, ids, binary, binarySha256, helperSha256, cwd, args, allowFiltered })), null, 2));
  try {
    const evaluated = await evaluateArchivedBackend(plan, (target, execution) => runCargo(target, { ...execution, signal: options.signal }), async (item, result) => {
      const stem = item.target === "large_directory_enqueue" ? "large-folder" : item.target;
      // Validate original captured stdout above; publish only password-redacted
      // channels, retaining their separation and hashes of the published bytes.
      const stdout = typeof result.stdout === "string" ? redact(result.stdout) : null;
      const stderr = typeof result.stderr === "string" ? redact(result.stderr) : null;
      const log = item.target === "large_directory_enqueue" ? stdout ?? redact(result.failure) : redact(result.output ?? result.failure);
      await writeFile(join(output, `${stem}.log`), log);
      if (stdout !== null) await writeFile(join(output, `${stem}.stdout.log`), stdout);
      if (stderr !== null) await writeFile(join(output, `${stem}.stderr.log`), stderr);
      const suite = { passed: result.passed, elapsedMs: result.elapsedMs, evidence: `${stem}.log`, evidenceDigest: sha256(log), exitCode: result.code ?? null, signal: result.signal ?? null, cancelled: !!result.cancelled, cleanupUnverified: !!result.cleanupUnverified, forcedRetirement: !!result.forcedRetirement, outputTruncated: result.outputTruncated, failure: result.failure, scenarios: result.scenarios ?? [], stdoutSha256: stdout !== null ? sha256(stdout) : null, stderrSha256: stderr !== null ? sha256(stderr) : null };
      executionSuites[item.target] = suite;
      if (Object.hasOwn(BACKEND_SUITES, item.target)) {
        const { stdoutSha256: _stdout, stderrSha256: _stderr, outputTruncated: _truncated, ...backendReport } = suite;
        report.suites[item.target] = backendReport;
      }
      console.log(item.target === "large_directory_enqueue" && result.passed ? "Large-folder acceptance: 1 real scenario passed" : `${item.target}: ${result.passed ? `${result.scenarios.length} real scenarios passed` : `FAILED; inspect ${stem}.log`}`);
    });
    assert.equal(evaluated.failures.length, 0, evaluated.failures.join("; "));
    assert.equal(Object.keys(evaluated.records).length, plan.length, "Incomplete ignored execution");
    await verifyPreparedArchive(prepared);
    const finalHelperSha256 = executableHash(plan[0].env["CARGO_BIN_EXE_mesh-talk-node"]);
    for (const item of plan) {
      assert.equal(executableHash(item.binary), item.binarySha256, "Ignored executable changed during execution");
      assert.equal(finalHelperSha256, item.helperSha256, "Ignored embedded CLI changed during execution");
    }
    const coverageFile = join(output, "coverage.lcov");
    const coverageResult = await runCargo("ignored-coverage", { command: ["cargo", "llvm-cov", "report", "--lcov", "--output-path", coverageFile], signal: options.signal });
    assert.ok(!coverageResult.failure && coverageResult.code === 0 && !coverageResult.cleanupUnverified, coverageResult.failure || "Ignored coverage reporting failed");
    const coverage = validateLcov(await readFile(coverageFile, "utf8"));
    await verifyPreparedArchive(prepared);
    await writeFile(join(output, "receipt.json"), JSON.stringify({ schema: prepared.manifest.schema, kind: "linux-ignored", channelRedaction: "recognizable-passwords-v1", identity: prepared.manifest.identity, archiveSha256: options.archiveSha256, manifestSha256: options.manifestSha256, adapterSha256: executableHash(fileURLToPath(import.meta.url)), featureResolutionSha256: prepared.manifest.featureResolutionSha256, inventoryFiles: prepared.manifest.inventoryFiles, selectedInventorySha256: sha256(await readFile(join(output, "ignored-inventory.json"))), suites: executionSuites, executedTests: plan.reduce((sum, item) => sum + item.expected.length, 0), coverage, coverageSha256: sha256(await readFile(coverageFile)) }, null, 2));
    verifyArchivedBackendEvidence(directory, output, { ...options, sourceSha: prepared.manifest.identity.sourceSha, runId: prepared.manifest.identity.runId, runAttempt: prepared.manifest.identity.runAttempt });
  } catch (error) { report.failure = String(error.message); await rm(join(output, "receipt.json"), { force: true }); throw error; }
  finally { report.finishedAt = new Date().toISOString(); await writeFile(join(output, "report.json"), JSON.stringify(report, null, 2)); }
  return report;
}

async function main() {
  const sourceSha = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
  assert.match(sourceSha, /^[0-9a-f]{40}$/);
  if (process.env.EVAL_SOURCE_SHA) assert.equal(sourceSha, process.env.EVAL_SOURCE_SHA, "Runner checkout differs from requested revision");
  const root = resolve(process.env.EVAL_OUTPUT_DIR ?? "target/evals/backend");
  await mkdir(root, { recursive: true });
  const report = { schema: 1, sourceSha, platform: process.platform, native: true, mocked: false, startedAt: new Date().toISOString(), suites: {}, kdf: "fast-test-kdf via existing self-dev-dependency; release binary unaffected" };
  try {
    const failures = await evaluateBackendSuites(runCargo, async (target, result) => {
      // The integration fixtures use generated throwaway accounts. Still redact
      // recognizable secrets before publishing diagnostics as public artifacts.
      const log = (result.output ?? result.failure).replace(/(--password\s+|password[=:]\s*)\S+/gi, "$1[REDACTED]");
      const file = `${target}.log`;
      await writeFile(join(root, file), log);
      const evidenceDigest = createHash("sha256").update(log).digest("hex");
      report.suites[target] = { passed: result.passed, elapsedMs: result.elapsedMs, evidence: file, evidenceDigest, exitCode: result.code ?? null, signal: result.signal ?? null, cancelled: !!result.cancelled, cleanupUnverified: !!result.cleanupUnverified, forcedRetirement: !!result.forcedRetirement, failure: result.failure, scenarios: result.scenarios ?? [] };
      console.log(`${target}: ${result.passed ? `${result.scenarios.length} real scenarios passed` : `FAILED; inspect ${file}`}`);
    });
    if (failures.length) {
      report.failures = failures;
      throw new Error(failures.join("; "));
    }
  } catch (error) {
    report.failure = String(error.message); process.exitCode = 1;
    console.error(report.failure);
  } finally {
    report.finishedAt = new Date().toISOString();
    await writeFile(join(root, "report.json"), JSON.stringify(report, null, 2));
  }
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  if (process.argv[2] === "--archive" || process.argv[2] === "--collect-linux") {
    try {
      const [mode, ...args] = process.argv.slice(2);
      if (mode === "--archive") {
        assert.equal(args.length, 4, "Usage: --archive ARCHIVE_DIR OUTPUT_DIR MANIFEST_SHA ARCHIVE_SHA");
        await runArchivedBackend(resolve(args[0]), resolve(args[1]), { manifestSha256: args[2], archiveSha256: args[3] });
      } else {
        assert.equal(args.length, 6, "Usage: --collect-linux METADATA_DIR DEFAULT_1_DIR DEFAULT_2_DIR IGNORED_DIR MANIFEST_SHA ARCHIVE_SHA");
        console.log(JSON.stringify(await collectLinuxWorkspace(resolve(args[0]), args.slice(1, 3).map(value=>resolve(value)), resolve(args[3]), { manifestSha256: args[4], archiveSha256: args[5], sourceSha: process.env.GITHUB_SHA, runId: process.env.GITHUB_RUN_ID, runAttempt: process.env.GITHUB_RUN_ATTEMPT, jobResults: { producer: process.env.MESH_LINUX_PRODUCER_RESULT, consumers: process.env.MESH_LINUX_CONSUMERS_RESULT, ignored: process.env.MESH_LINUX_IGNORED_RESULT } })));
      }
    } catch(error) { console.error(error.stack); process.exitCode=1; }
  } else if (process.argv[2] === "--validate-large-folder" && process.argv.length === 4) {
    validateBackendOutput(await readFile(process.argv[3], "utf8"), [LARGE_FOLDER_SCENARIO], { allowFiltered: true });
    console.log("Large-folder acceptance: 1 real scenario passed");
  } else {
    assert.equal(process.argv.length, 2, "Usage: backend-runner.mjs [--validate-large-folder FILE]");
    await main();
  }
}
