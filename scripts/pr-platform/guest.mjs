/** Shared bounded guest execution. Selectors are argv data, never shell fragments. */
import { createHash } from "node:crypto";
import { createReadStream, existsSync } from "node:fs";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { validatePlan } from "./plan.mjs";
import { execute } from "../validation/stage.mjs";
import { selectedRustPackages } from "./rust-packages.mjs";

export function rustTestCount(output) {
  return [...output.matchAll(/test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed;/gu)]
    .reduce((total, match) => total + Number(match[1]) + Number(match[2]), 0);
}

export async function digestFile(filename) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(filename)) hash.update(chunk);
  return hash.digest("hex");
}

export async function runCommand(program, args, { cwd, env = process.env, timeoutMs = 1_800_000, logPath, detached = process.platform !== "win32", maxOutputBytes = 128 * 1024 * 1024, outputLimitIsFailure = false, signal } = {}) {
  if (!Number.isSafeInteger(maxOutputBytes) || maxOutputBytes <= 0) throw Error("Output byte limit must be a positive safe integer");
  const started = Date.now();
  let output = "", bytes = 0, timedOut = false, failure, outputTruncated = false;
  // Structured consumers require stream provenance. A single shared raw-byte
  // window bounds both channel captures; the existing combined log is retained.
  const captured = [];
  let first = 0, retainedBytes = 0;
  const capture = (chunk, channel) => {
    if (!chunk.length) return;
    captured.push({ chunk, channel });
    retainedBytes += chunk.length;
    while (retainedBytes > maxOutputBytes) {
      outputTruncated = true;
      const item = captured[first];
      const excess = retainedBytes - maxOutputBytes;
      if (item.chunk.length <= excess) {
        retainedBytes -= item.chunk.length;
        captured[first++] = undefined;
      } else {
        item.chunk = Buffer.from(item.chunk.subarray(excess));
        retainedBytes -= excess;
      }
    }
    if (first > 256 && first * 2 >= captured.length) { captured.splice(0, first); first = 0; }
  };
  const channels = () => {
    const records = captured.slice(first);
    const text = channel => Buffer.concat(records.filter(item => item.channel === channel).map(item => item.chunk)).toString();
    return { stdout: text("stdout"), stderr: text("stderr"), outputTruncated };
  };
  const controller = new AbortController();
  let timer;
  const stop = () => { clearTimeout(timer); controller.abort(); };
  signal?.addEventListener("abort", stop, { once: true });
  if (signal?.aborted) stop();
  process.on("SIGINT", stop); process.on("SIGTERM", stop);
  timer = setTimeout(() => { timedOut = true; stop(); }, timeoutMs);
  try {
    const executable = program === "cargo" && env.MESH_PR_CARGO ? env.MESH_PR_CARGO : program;
    // Resolve npm exactly as the existing guest owner does, including npm_execpath.
    let command = [executable, ...args];
    if (program === "npm") {
      const npm = [env.npm_execpath, path.join(path.dirname(process.execPath), "node_modules/npm/bin/npm-cli.js"), path.resolve(path.dirname(process.execPath), "../lib/node_modules/npm/bin/npm-cli.js")].find(candidate => candidate && existsSync(candidate));
      if (!npm) return { code: null, timedOut: false, output: "", ...channels(), error: "Cannot locate npm-cli.js beside the installed Node executable" };
      command = [process.execPath, npm, ...args];
    }
    const result = await execute(command, { cwd, env, logPath, signal: controller.signal, detached, notifyOwnerOnAbort: true, onOutput(chunk, channel) {
      bytes += chunk.length;
      if (bytes > maxOutputBytes) outputTruncated = true;
      if (outputLimitIsFailure && bytes > maxOutputBytes) { failure ||= "Command output exceeded evidence limit"; stop(); return; }
      output += chunk.toString();
      if (Buffer.byteLength(output) > maxOutputBytes) {
        outputTruncated = true;
        const keep = Math.floor(maxOutputBytes / 4);
        output = keep ? output.slice(-keep) : "";
      }
      capture(chunk, channel);
    } });
    return { code: controller.signal.aborted && !timedOut ? 130 : result.error && result.exitCode === 0 ? 1 : result.exitCode, timedOut, output, ...channels(), error: failure || result.error, signal: result.signal, cleanupUnverified: result.cleanupUnverified, forcedRetirement: !!result.forcedRetirement, cancelled: controller.signal.aborted && !timedOut && !failure, elapsedMs: Date.now() - started };
  } catch (error) {
    return { code: controller.signal.aborted ? 130 : null, timedOut, output, ...channels(), error: error.message, cancelled: controller.signal.aborted && !timedOut, elapsedMs: Date.now() - started };
  } finally {
    clearTimeout(timer); signal?.removeEventListener("abort", stop);
    process.off("SIGINT", stop); process.off("SIGTERM", stop);
  }
}


/** Machine acceptance never parses the interleaved human log or a partial view. */
export function requireCommandStdout(result) {
  if (typeof result.stdout !== "string") throw Error("Command stdout capture is unavailable");
  if (result.outputTruncated !== false) throw Error("Command output capture is truncated or unverified");
  return result.stdout;
}

/** Normalize Playwright's JSON reporter (rootDir-relative files) to frontend paths. */
export function playwrightEvidence(report, frontendDirectory) {
  if (!report.config?.rootDir || !Array.isArray(report.suites) || !Array.isArray(report.errors)) throw Error("Malformed Playwright JSON report");
  const paths = /^[A-Za-z]:[\\/]/u.test(frontendDirectory) ? path.win32 : path.posix;
  const tests = [];
  function visit(suites) {
    for (const suite of suites) {
      for (const spec of suite.specs || []) {
        if (typeof spec.file !== "string" || !Array.isArray(spec.tests)) throw Error("Malformed Playwright spec");
        const file = paths.relative(frontendDirectory, paths.resolve(report.config.rootDir, spec.file)).replaceAll("\\", "/");
        for (const test of spec.tests) {
          if (!Array.isArray(test.results)) throw Error("Missing Playwright test results");
          tests.push({ id: spec.id, file, project: test.projectName, expectedStatus: test.expectedStatus, status: test.status, ok: spec.ok === true,
            results: test.results.map(result => ({ status: result.status, errors: result.errors?.length || 0 })) });
        }
      }
      visit(suite.suites || []);
    }
  }
  visit(report.suites);
  return { errors: report.errors.length, stats: { expected: report.stats?.expected, skipped: report.stats?.skipped, unexpected: report.stats?.unexpected, flaky: report.stats?.flaky }, tests };
}

export function validatePlaywrightEvidence(evidence, selected) {
  if (!evidence || evidence.errors !== 0 || !Array.isArray(evidence.tests)) throw Error("Missing or failed Playwright JSON evidence");
  const counts = new Map(selected.map(file => [file, 0]));
  const ids = new Set();
  let expected = 0;
  let skipped = 0;
  for (const test of evidence.tests) {
    if (!counts.has(test.file)) throw Error(`Unselected Playwright spec: ${test.file}`);
    if (!test.id || ids.has(test.id) || test.project !== "chromium" || !test.ok || !Array.isArray(test.results)) throw Error("Invalid Playwright test identity or outcome");
    ids.add(test.id);
    if (test.results.some(result => !["passed", "skipped"].includes(result.status) || result.errors !== 0)) throw Error(`Failed or interrupted Playwright test: ${test.file}`);
    if (test.status === "expected" && test.expectedStatus === "passed" && test.results.length > 0 && test.results.every(result => result.status === "passed")) {
      expected++;
      counts.set(test.file, counts.get(test.file) + 1);
    } else if (test.status === "skipped" && test.results.every(result => result.status === "skipped")) skipped++;
    else throw Error(`Incomplete Playwright execution: ${test.file}`);
  }
  if (evidence.stats?.expected !== expected || evidence.stats?.skipped !== skipped || evidence.stats?.unexpected !== 0 || evidence.stats?.flaky !== 0) throw Error("Inconsistent Playwright structured counts");
  for (const [file, passed] of counts) if (passed === 0) throw Error(`Selected Playwright spec ran zero passing tests: ${file}`);
  return counts;
}

export function expectedSteps(plan) {
  const steps = [{ name: "rust-version", program: "rustc", args: ["--version"] }, { name: "cargo-version", program: "cargo", args: ["--version"] }, { name: "node-version", program: "node", args: ["--version"] }];
  if (plan.nodeDiagnostics.length) steps.push({ name: "workflow-diagnostics", program: "node", args: ["--test", ...plan.nodeDiagnostics] });
  const needsNpm = plan.frontendBuild || plan.uiSpecs.length;
  if (needsNpm) steps.push({ name: "npm-ci", program: "npm", args: ["ci", "--no-audit", "--no-fund"], frontend: true });
  if (plan.frontendBuild) steps.push({ name: "frontend-build", program: "npm", args: ["run", "build"], frontend: true });
  for (const pkg of selectedRustPackages(plan)) {
    steps.push({ name: `${pkg.kind}-clippy`, program: "cargo", args: ["clippy", "--locked", "-p", pkg.package, "--all-targets", "--", "-D", "warnings"] });
    for (const filter of plan[pkg.filters] ?? []) steps.push({ name: `${pkg.kind}:${filter}`, kind: "rust", program: "cargo", args: ["test", "--locked", "-p", pkg.package, "--lib", filter, "--", "--nocapture"] });
    for (const target of pkg.targets ? plan[pkg.targets] : []) steps.push({ name: `${pkg.kind}-test:${target}`, kind: "rust", program: "cargo", args: ["test", "--locked", "-p", pkg.package, "--test", target, "--", "--nocapture"] });
  }
  if (plan.uiSpecs.length) {
    const cli = "node_modules/playwright/cli.js";
    steps.push({ name: "browser-install", program: "node", args: [cli, "install", "--only-shell", "chromium"], frontend: true });
    steps.push({ name: "selected-playwright", kind: "ui", program: "node", args: [cli, "test", "--project=chromium", "--workers=1", "--retries=0", "--reporter=list,json", "--output", "{evidence}/playwright", ...plan.uiSpecs], frontend: true });
  }
  return steps;
}

export async function executePlan(plan, { cwd = process.cwd(), env = process.env, run = runCommand, timeoutMs = 1_800_000, reportDir } = {}) {
  validatePlan(plan);
  const steps = expectedSteps(plan);
  const started = Date.now();
  const results = [];
  for (const [index, step] of steps.entries()) {
    const remaining = timeoutMs - (Date.now() - started);
    let result;
    let ui;
    let uiError;
    const uiReport = path.join(reportDir || path.join(cwd, "target"), "playwright.json");
    try {
      if (step.kind === "ui") { await mkdir(path.dirname(uiReport), { recursive: true }); await rm(uiReport, { force: true }); }
      result = remaining > 0 ? await run(step.program === "node" ? process.execPath : step.program,
        step.args.map(arg => arg === "{evidence}/playwright" ? path.join(reportDir || path.join(cwd, "target"), "playwright") : arg), {
        cwd: step.frontend ? path.join(cwd, "frontend") : cwd,
        env: { ...env, CI: "1", EVAL_SOURCE_SHA: plan.sourceSha, RUST_TEST_THREADS: "4", CARGO_INCREMENTAL: "0", ...(step.kind === "ui" ? { PLAYWRIGHT_JSON_OUTPUT_FILE: uiReport } : {}) }, shell: false, timeoutMs: remaining,
        logPath: reportDir ? path.join(reportDir, `step-${index}.log`) : undefined,
      }) : { code: null, timedOut: true, output: "", error: "Platform deadline exhausted" };
    } catch (error) { result = { code: null, output: "", error: error.message }; }
    if (step.kind === "ui" && result.code === 0 && !result.timedOut) {
      try { ui = playwrightEvidence(JSON.parse(await readFile(uiReport, "utf8")), path.join(cwd, "frontend")); validatePlaywrightEvidence(ui, plan.uiSpecs); }
      catch (error) { uiError = error.message; }
    }
    let count, captureError;
    if (step.kind === "rust" && result.code === 0 && !result.error && !result.timedOut && !result.cancelled && !result.cleanupUnverified) {
      try { count = rustTestCount(requireCommandStdout(result)); }
      catch (failure) { captureError = failure.message; }
    }
    const error = result.error || uiError || captureError || (result.cleanupUnverified ? "Descendant retirement could not be verified" : result.cancelled ? "Platform execution cancelled" : result.timedOut ? "Command timed out" : result.code !== 0 ? `Command exited ${result.code}` : step.kind === "rust" && count === 0 ? "Selector ran zero Rust tests" : undefined);
    results.push({ name: step.name, kind: step.kind, program: step.program, args: step.args, code: result.code, timedOut: !!result.timedOut, cancelled: !!result.cancelled, cleanupUnverified: !!result.cleanupUnverified, forcedRetirement: !!result.forcedRetirement, testCount: count, ui, ok: !error, error, elapsedMs: result.elapsedMs, outputTail: result.output.slice(-4000) });
    if (result.cancelled || result.cleanupUnverified) break;
  }
  return { schema: 1, sourceSha: plan.sourceSha, archiveSha: plan.archiveSha, platform: process.platform, architecture: os.arch(), cancelled: results.some(result => result.cancelled), cleanupUnverified: results.some(result => result.cleanupUnverified), scope: "Affected workflow diagnostics, selected Rust package lint/tests and explicitly selected browser specs; no ASan or native UI certification", ok: results.every(result => result.ok), elapsedMs: Date.now() - started, results };
}

async function main() {
  const [planFile, reportFile] = process.argv.slice(2);
  if (!planFile || !reportFile) throw Error("Usage: guest.mjs PLAN.json REPORT.json");
  const plan = JSON.parse(await readFile(planFile, "utf8"));
  await mkdir(path.dirname(reportFile), { recursive: true });
  let report;
  try {
    if (!process.env.MESH_PR_ARCHIVE || await digestFile(process.env.MESH_PR_ARCHIVE) !== plan.archiveSha) throw Error("Source archive digest mismatch");
    report = await executePlan(plan, { reportDir: path.dirname(reportFile), timeoutMs: Number(process.env.MESH_PR_PLATFORM_TIMEOUT_SECONDS || 1800) * 1000 });
  } catch (error) { report = { ok: false, sourceSha: plan.sourceSha, platform: process.platform, architecture: os.arch(), error: error.message, results: [] }; }
  await writeFile(reportFile, `${JSON.stringify(report, null, 2)}\n`);
  if (!report.ok) process.exitCode = 1;
}
if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) main().catch(error => { console.error(error.message); process.exitCode = 1; });
