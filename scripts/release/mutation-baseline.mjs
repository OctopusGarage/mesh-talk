// Feature-local producer/consumer protocol. The shared command owner bounds work;
// Linux kernel adoption qualifies native-tool retirement independently of groups.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { spawnSync } from "node:child_process";
import { appendFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { runCommand, requireCommandStdout } from "../pr-platform/guest.mjs";
import { inventory, verifyExecution, verifyTargets, versions } from "../ci/windows-coverage.mjs";
import { createMutationPlan, evidenceDigest, mutationBudgets, mutationToolVersion, mutationShard, parseSelectedMutants, validateMutationReport, verifyBaselineReceipt, verifyMutationPlan, verifyMutationOutcomes } from "./mutation-evidence.mjs";

const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const read = filename => JSON.parse(readFileSync(filename, "utf8"));
const write = (filename, value) => { writeFileSync(`${filename}.partial`, `${JSON.stringify(value, null, 2)}\n`); renameSync(`${filename}.partial`, filename); };
const fileDigest = filename => sha(readFileSync(filename));
const env = () => ({ ...process.env, CARGO_INCREMENTAL: "1", NEXTEST_EXPERIMENTAL_LIBTEST_JSON: "1" });
const tool = (name, args, cwd) => {
  const result = spawnSync(name, args, { cwd, env: env(), encoding: "utf8", timeout: 30000, maxBuffer: 4 * 1024 * 1024 });
  assert.ok(!result.error && result.status === 0, `${name} identity failed: ${result.error || result.stderr}`);
  return result.stdout.trim();
};
const root = () => tool("git", ["rev-parse", "--show-toplevel"], process.cwd());
const attempt = () => {
  assert.match(process.env.GITHUB_RUN_ATTEMPT || "", /^[1-9]\d*$/u, "run attempt required");
  return process.env.GITHUB_RUN_ATTEMPT;
};
const trusted = name => {
  const value = process.env[name];
  assert.ok(value, `trusted ${name} output required`);
  return value;
};
function output(values) {
  if (process.env.GITHUB_OUTPUT) for (const [key, value] of Object.entries(values)) appendFileSync(process.env.GITHUB_OUTPUT, `${key}=${typeof value === "string" ? value : JSON.stringify(value)}\n`);
}

export function sourceIdentity(directory) {
  const source = tool("git", ["rev-parse", "HEAD"], directory);
  assert.equal(source, process.env.GITHUB_SHA, "mutation checkout differs from requested source");
  assert.equal(tool("git", ["status", "--porcelain", "--untracked-files=normal"], directory), "", "mutation source must be clean");
  assert.match(process.env.GITHUB_RUN_ID || "", /^[1-9]\d*$/u, "run ID required");
  const files = ["Cargo.toml", "Cargo.lock", ".cargo/config.toml", ".config/nextest.toml"];
  const owners = ["mutation-baseline.mjs", "mutation-evidence.mjs", "run-mutants.mjs", "mutation-process-owner.py"];
  return { sourceSha: source, tree: tool("git", ["rev-parse", "HEAD^{tree}"], directory), runId: process.env.GITHUB_RUN_ID,
    workflow: process.env.GITHUB_WORKFLOW || "", platform: process.platform, arch: process.arch,
    files: Object.fromEntries(files.map(file => [file, fileDigest(path.join(directory, file))])),
    owners: Object.fromEntries(owners.map(file => [file, fileDigest(fileURLToPath(new URL(file, import.meta.url)))])) };
}
export function mutationIdentity(directory) {
  const identity = { ...sourceIdentity(directory), rustc: tool(process.env.RUSTC || "rustc", ["-Vv"], directory), cargo: tool("cargo", ["-V"], directory),
    nextest: tool("cargo", ["nextest", "--version"], directory), mutants: tool("cargo", ["mutants", "--version"], directory), node: process.version,
    profile: "mutants", testThreads: 4, retries: 0,
    environment: Object.fromEntries(Object.entries(env()).filter(([key]) => /^(?:RUSTFLAGS|RUSTDOCFLAGS|RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|CARGO_ENCODED_RUSTFLAGS|CARGO_TARGET_DIR|CARGO_BUILD_TARGET|CARGO_PROFILE_.*|CARGO_INCREMENTAL|CC|CFLAGS|CXX|CXXFLAGS|AR|OPENSSL_SRC_PERL|PERL|STATIC_VCRUNTIME|MESH_TALK_(?:GOOGLE_CLIENT_ID|GOOGLE_CLIENT_SECRET|UPDATER_PUBLIC_KEY|BUILD_TAG))$/u.test(key)).sort(([a], [b]) => a.localeCompare(b)).map(([key, value]) => [key, evidenceDigest(value)])) };
  assert.equal(identity.nextest.split(/\s/u).slice(0, 2).join(" "), `cargo-nextest ${versions.nextest}`, "pinned Nextest required");
  assert.equal(identity.mutants, `cargo-mutants ${mutationToolVersion}`, "pinned cargo-mutants required");
  return identity;
}
function assets(directory) {
  const target = path.join(directory, "frontend/dist");
  const entries = [];
  const visit = (base, relative = "") => {
    for (const item of readdirSync(base, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      assert.ok(!item.isSymbolicLink(), "generated assets must not contain symlinks");
      const name = path.join(relative, item.name);
      if (item.isDirectory()) visit(path.join(base, item.name), name);
      else { assert.ok(item.isFile(), "unsupported generated asset"); entries.push([name, fileDigest(path.join(base, item.name))]); }
    }
  };
  if (existsSync(target)) visit(target);
  return evidenceDigest(entries);
}
// A shared source/config cannot hide a changed native package cohort between
// producer and workers. The plan runs before native setup, so bind this at baseline.
const nativeHostPackages = ["libwebkit2gtk-4.1-dev", "libgtk-3-dev", "libayatana-appindicator3-dev", "librsvg2-dev"];
function nativeDependencies(directory, names) {
  assert.ok(Array.isArray(names), "native package declaration required");
  assert.ok(names.length === 0 || evidenceDigest(names) === evidenceDigest(nativeHostPackages), "unknown native package cohort");
  return names.length ? { names, versions: tool("dpkg-query", ["--show", "--showformat=${binary:Package}=${Version}\n", ...names], directory),
    compiler: tool("cc", ["--version"], directory), pkgConfig: tool("pkg-config", ["--version"], directory) } : { names: [] };
}
export function verifyMutationRetirement(result, acknowledgement, nonce) {
  assert.equal(acknowledgement?.schema, 1, "missing or invalid mutation retirement acknowledgement");
  assert.equal(acknowledgement.nonce, nonce, "mutation retirement nonce mismatch");
  assert.equal(acknowledgement.retired, true, "mutation child retirement unverified");
  assert.ok(Number.isInteger(acknowledgement.exitCode) && acknowledgement.exitCode >= 0 && acknowledgement.exitCode <= 255, "invalid mutation leader status");
  if (!result.cancelled && !result.timedOut) assert.equal(acknowledgement.exitCode, result.code, "mutation leader status mismatch");
}
// Linux native tools can create independent groups. The feature-local kernel
// subreaper owns retirement; the shared runner still owns deadlines/logging.
export async function runMutationTool(args, directory, evidence, phase, seconds, allowed = [0], signal) {
  mkdirSync(evidence, { recursive: true });
  console.log(`[mutation:${phase}] started; deadline ${seconds}s`);
  const heartbeat = setInterval(() => console.log(`[mutation:${phase}] still running`), 60000);
  let result;
  try {
    const options = { cwd: directory, env: env(), timeoutMs: seconds * 1000, signal,
      logPath: path.join(evidence, `${phase}.log`), maxOutputBytes: 128 * 1024 * 1024, outputLimitIsFailure: true };
    if (process.platform === "linux") {
      const nonce = randomBytes(32).toString("hex");
      const privateDirectory = mkdtempSync(path.join(path.resolve(evidence), `${phase}-retirement-`));
      const acknowledgement = path.join(privateDirectory, "retired.json");
      result = await runCommand("python3", [fileURLToPath(new URL("mutation-process-owner.py", import.meta.url)), acknowledgement, nonce, "--", "cargo", ...args], { ...options, terminationGraceMs: 20000 });
      try { verifyMutationRetirement(result, read(acknowledgement), nonce); }
      catch (error) { result.cleanupUnverified = true; result.error ||= `${error.message}; owner exit ${result.code}: ${(result.stderr || "").slice(-4096)}`; }
    } else {
      result = await runCommand("cargo", args, options);
    }
  } finally { clearInterval(heartbeat); }
  const { output: _output, stdout: _stdout, stderr: _stderr, ...details } = result;
  write(path.join(evidence, `${phase}-result.json`), { args, ...details });
  assert.ok(!result.timedOut && !result.cancelled && !result.cleanupUnverified && !result.error && result.outputTruncated === false, `mutation ${phase} process failed: ${result.error || result.code}`);
  assert.ok(allowed.includes(result.code), `mutation ${phase} exit indicates failure: ${result.code}`);
  console.log(`[mutation:${phase}] completed in ${(result.elapsedMs / 1000).toFixed(3)}s`);
  return result;
}
const command = runMutationTool;
export const mutationOptions = shard => ["--test-tool", "nextest", "--cargo-arg=--locked", "--cargo-test-arg=--profile=mutants", "--workspace", "--test-workspace", "true", "--in-place", "--profile", "mutants", "--no-shuffle", "--shard", shard, "--sharding", "round-robin", "--timeout", String(mutationBudgets.testSeconds), "--build-timeout", String(mutationBudgets.compileSeconds), "--output", ".."];
async function selection(directory, shard, evidence) {
  const result = await command(["mutants", "--list", "--json", ...mutationOptions(shard)], directory, evidence, "selection", 180);
  return parseSelectedMutants(requireCommandStdout(result), result.code);
}
function expectedPlan() { return { receiptDigest: trusted("MUTATION_PLAN_DIGEST"), producerAttempt: trusted("MUTATION_PLAN_ATTEMPT") }; }
function loadPlan(directory, identity) {
  const envelope = read(path.join(directory, "plan.json"));
  const expected = expectedPlan();
  assert.equal(evidenceDigest(envelope), expected.receiptDigest, "mutation plan digest mismatch");
  assert.equal(envelope.role, "mutation-plan", "invalid plan role");
  assert.equal(envelope.producerAttempt, expected.producerAttempt, "plan producer attempt mismatch");
  assert.ok(Number(envelope.producerAttempt) <= Number(attempt()), "future plan attempt");
  assert.deepEqual(envelope.identity, identity, "plan source/tool/config identity mismatch");
  verifyMutationPlan(envelope.plan);
  assert.equal(envelope.plan.runNumber, Number(process.env.GITHUB_RUN_NUMBER), "mutation rotation changed");
  return envelope;
}
export async function produceMutationPlan(directory) {
  mkdirSync(directory, { recursive: true });
  write(path.join(directory, "admission.json"), { phase: "plan", startedAt: new Date().toISOString() });
  const workspace = root();
  assert.ok(!existsSync(path.join(directory, "plan.json")), "prior mutation plan cannot be reused");
  const identity = mutationIdentity(workspace);
  const runNumber = Number(process.env.GITHUB_RUN_NUMBER);
  const selected = await selection(path.join(workspace, "src-tauri"), mutationShard(runNumber), directory);
  const envelope = { schema: 1, role: "mutation-plan", identity, runnerImage: process.env.ImageVersion || "", producerAttempt: attempt(), plan: createMutationPlan(runNumber, selected) };
  assert.deepEqual(mutationIdentity(workspace), identity, "source changed during mutation selection");
  write(path.join(directory, "plan.json"), envelope);
  output({ selected: selected.length, matrix: { worker: envelope.plan.workers.length ? envelope.plan.workers.map(item => item.worker) : [0] }, workers: envelope.plan.workers.length,
    digest: evidenceDigest(envelope), producer_attempt: envelope.producerAttempt, rust: identity.rustc.split(/\s/u)[1], node: process.versions.node });
  return envelope;
}
const nextestOptions = ["--workspace", "--locked", "--cargo-profile", "mutants", "--profile", "mutants", "--user-config-file", "none"];
export async function produceMutationBaseline(planDirectory, directory) {
  assert.equal(process.platform, "linux", "shared native mutation baseline requires Linux child ownership");
  mkdirSync(directory, { recursive: true });
  write(path.join(directory, "admission.json"), { phase: "baseline", startedAt: new Date().toISOString() });
  const workspace = root(), identity = mutationIdentity(workspace);
  const envelope = loadPlan(planDirectory, identity);
  assert.ok(envelope.plan.selected.length > 0, "empty mutation sample needs no baseline");
  assert.ok(!existsSync(path.join(directory, "receipt.json")), "prior mutation baseline cannot be reused");
  const generatedAssets = assets(workspace);
  const compileDeadline = performance.now() + mutationBudgets.compileSeconds * 1000;
  const metadataResult = await command(["metadata", "--locked", "--format-version", "1"], workspace, directory, "metadata", 180);
  const metadata = JSON.parse(requireCommandStdout(metadataResult));
  const nativeNames = process.platform === "linux" && metadata.packages.some(item => item.name === "tauri") ? nativeHostPackages : [];
  const nativePackages = nativeDependencies(workspace, nativeNames);
  const remainingCompile = (compileDeadline - performance.now()) / 1000;
  assert.ok(remainingCompile > 0, "baseline compile budget exhausted before listing");
  const listed = await command(["nextest", "list", ...nextestOptions, "--message-format", "json"], workspace, directory, "compile", remainingCompile);
  const listing = JSON.parse(requireCommandStdout(listed));
  const rows = inventory(listing);
  assert.ok(rows.some(item => item.selected) && rows.every(item => item.selected === !item.ignored), "baseline has empty or hidden selection");
  verifyTargets(listing, metadata);
  write(path.join(directory, "inventory.json"), rows);
  const executed = await command(["nextest", "run", ...nextestOptions, "--test-threads", "4", "--retries", "0", "--fail-fast", "--no-tests", "fail", "--message-format", "libtest-json-plus", "--message-format-version", "0.1"], workspace, directory, "execution", mutationBudgets.testSeconds);
  const execution = requireCommandStdout(executed);
  verifyExecution(execution, rows);
  writeFileSync(path.join(directory, "execution.jsonl"), execution);
  assert.deepEqual(mutationIdentity(workspace), identity, "source changed during mutation baseline");
  assert.equal(assets(workspace), generatedAssets, "generated assets changed during baseline");
  assert.deepEqual(nativeDependencies(workspace, nativeNames), nativePackages, "native package cohort changed during baseline");
  const receipt = { schema: 1, role: "mutation-baseline", identity, runnerImage: process.env.ImageVersion || "", producerAttempt: attempt(), planDigest: evidenceDigest(envelope.plan), generatedAssets, nativePackages,
    tests: rows.length, passed: rows.filter(item => item.selected).length, ignored: rows.filter(item => item.ignored).length,
    files: { inventory: fileDigest(path.join(directory, "inventory.json")), execution: fileDigest(path.join(directory, "execution.jsonl")) } };
  write(path.join(directory, "receipt.json"), receipt);
  output({ digest: evidenceDigest(receipt), producer_attempt: receipt.producerAttempt });
  return receipt;
}
function loadBaseline(directory, identity, plan) {
  const receipt = read(path.join(directory, "receipt.json"));
  const expected = { identity, receiptDigest: trusted("MUTATION_BASELINE_DIGEST"), producerAttempt: trusted("MUTATION_BASELINE_ATTEMPT"), planDigest: evidenceDigest(plan) };
  verifyBaselineReceipt(receipt, expected);
  assert.ok(Number(receipt.producerAttempt) <= Number(attempt()), "future baseline attempt");
  assert.equal(fileDigest(path.join(directory, "inventory.json")), receipt.files.inventory, "baseline inventory digest mismatch");
  assert.equal(fileDigest(path.join(directory, "execution.jsonl")), receipt.files.execution, "baseline execution digest mismatch");
  const rows = read(path.join(directory, "inventory.json"));
  assert.equal(rows.length, receipt.tests, "baseline inventory count mismatch");
  assert.ok(rows.every(item => item.selected === !item.ignored), "baseline hidden filters");
  assert.equal(rows.filter(item => item.selected).length, receipt.passed, "baseline selected count mismatch");
  verifyExecution(readFileSync(path.join(directory, "execution.jsonl"), "utf8"), rows);
  assert.equal(assets(root()), receipt.generatedAssets, "worker generated assets differ from baseline");
  assert.deepEqual(nativeDependencies(root(), receipt.nativePackages.names), receipt.nativePackages, "worker native package cohort differs from baseline");
  return { externalBaseline: receipt, expectedBaseline: expected };
}
export async function runMutationWorker(planDirectory, baselineDirectory, directory, index) {
  assert.equal(process.platform, "linux", "shared native mutation worker requires Linux child ownership");
  mkdirSync(directory, { recursive: true });
  write(path.join(directory, "admission.json"), { phase: "worker", index, startedAt: new Date().toISOString() });
  const workspace = root(), identity = mutationIdentity(workspace);
  const envelope = loadPlan(planDirectory, identity), plan = envelope.plan;
  const worker = plan.workers[index];
  assert.ok(Number.isSafeInteger(index) && worker && worker.selected.length > 0 && worker.selected.length <= mutationBudgets.maxMutants, "invalid or oversized mutation worker");
  assert.ok(!existsSync(path.join(directory, "worker.json")), "prior worker receipt cannot be reused");
  const baseline = loadBaseline(baselineDirectory, identity, plan);
  const selected = await selection(path.join(workspace, "src-tauri"), worker.shard, directory);
  assert.deepEqual(selected, worker.selected, "actual mutation selection differs from bounded plan");
  let result;
  try {
    result = await command(["mutants", ...mutationOptions(worker.shard), "--baseline", "skip", "--", "--test-threads=4"], path.join(workspace, "src-tauri"), directory, "mutants", worker.selected.length * (mutationBudgets.compileSeconds + mutationBudgets.testSeconds) + 60, [0, 2]);
  } finally {
    const reports = path.join(workspace, "mutants.out");
    if (existsSync(reports)) cpSync(reports, path.join(directory, "raw"), { recursive: true });
  }
  const report = read(path.join(workspace, "mutants.out/outcomes.json"));
  const summary = validateMutationReport(selected.length, report, result.code, baseline);
  verifyMutationOutcomes(selected, report);
  assert.deepEqual(mutationIdentity(workspace), identity, "in-place mutation did not restore source identity");
  const record = { schema: 1, worker: index, identity, runnerImage: process.env.ImageVersion || "", producerAttempt: attempt(), selected, report, exitCode: result.code, summary,
    planDigest: evidenceDigest(plan), baselineDigest: evidenceDigest(baseline.externalBaseline) };
  write(path.join(directory, "worker.json"), record);
  if (summary.missed) console.log(`::warning::${summary.missed} surviving mutations need review; see raw report`);
  return record;
}
