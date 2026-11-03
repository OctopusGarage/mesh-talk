#!/usr/bin/env node
/** Focused development runner. Required PR, platform and review gates remain separate. */
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { readFile, mkdir, mkdtemp, rm, lstat, readlink, realpath } from "node:fs/promises";
import { createHash, randomUUID } from "node:crypto";
import { buildAffectedPlan, isFrontendGlobal } from "./affected.mjs";
import { buildFrontendGraph } from "./frontend-graph.mjs";
import { loadContracts } from "./contracts.mjs";
import { runStage } from "./stage.mjs";
import { runCommand, rustTestCount, requireCommandStdout } from "../pr-platform/guest.mjs";
import { buildStaticChecks, phasePlan } from "./checks.mjs";
import { frontendEnvironmentIdentity } from "./stage-inputs.mjs";
import { loadRequirements, selectRequirements } from "./pr-scenarios.mjs";
import { createFeedback, completePhase, finishFeedback, observeCommands, writeFeedback } from "./feedback.mjs";

function git(root, args) {
  const result = spawnSync("git", args, {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
  });
  if (result.status !== 0)
    throw Error(result.stderr?.trim() || "Git source discovery failed");
  return result.stdout;
}
const nulList = (output) => output.split("\0").filter(Boolean);
export function discoverChanges(root, base = "HEAD") {
  if (!base || base.startsWith("-") || /[\x00-\x1f]/u.test(base))
    throw Error("Invalid comparison base");
  const resolved = git(root, ["merge-base", "HEAD", base]).trim();
  const files = [
    ...new Set([
      ...nulList(
        git(root, [
          "diff",
          "--name-only",
          "-z",
          "--no-renames",
          resolved,
          "--",
        ]),
      ),
      ...nulList(
        git(root, ["ls-files", "--others", "--exclude-standard", "-z"]),
      ),
    ]),
  ].sort();
  const deleted = nulList(git(root, ["diff", "--name-only", "-z", "--no-renames", "--diff-filter=D", resolved, "--"]));
  return { base: resolved, files, deleted };
}
export function parseOptions(args) {
  const options = {
    base: "HEAD",
    run: false,
    json: false,
    owners: [],
    reason: "",
    checks: false,
    managed: false,
    browser: [],
    native: [],
    report: undefined,
  };
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (["--run", "--json", "--checks", "--managed"].includes(arg)) {
      options[arg.slice(2)] = true;
      continue;
    }
    if (["--base", "--owner", "--reason", "--browser", "--native", "--report"].includes(arg)) {
      const value = args[++i];
      if (!value || value.startsWith("-") || /[\x00-\x1f]/u.test(value))
        throw Error(`${arg} requires a literal value`);
      if (arg === "--owner") options.owners.push(value);
      else if (arg === "--browser" || arg === "--native") options[arg.slice(2)].push(value);
      else options[arg.slice(2)] = value;
    } else throw Error(`Unknown option: ${arg}`);
  }
  if ((options.owners.length || options.browser.length || options.native.length) && !options.reason.trim())
    throw Error("Explicit owners/browser/native evidence requires --reason");
  if (options.report && !options.run) throw Error("--report requires --run; previews are read-only");
  return options;
}

export async function createPlan(root, options) {
  const changes = discoverChanges(root, options.base);
  const contracts = await loadContracts(root);
  let frontend;
  if (
    changes.files.some((file) => file.startsWith("frontend/src/") || isFrontendGlobal(file)) ||
    options.owners.some((owner) => owner.startsWith("frontend-"))
  ) {
    const current = new Map();
    const previous = new Map();
    const tracked = nulList(
      git(root, [
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
        "frontend/src",
      ]),
    );
    for (const file of new Set(tracked)) {
      try {
        current.set(file, await readFile(path.join(root, file), "utf8"));
      } catch (error) {
        if (error.code !== "ENOENT") throw error;
      }
    }
    for (const file of nulList(
      git(root, [
        "ls-tree",
        "-r",
        "--name-only",
        "-z",
        changes.base,
        "--",
        "frontend/src",
      ]),
    )) {
      // Existing files provide resolution targets; only changed files need their former edges.
      previous.set(
        file,
        changes.files.includes(file)
          ? git(root, ["show", `${changes.base}:${file}`])
          : "",
      );
    }
    frontend = buildFrontendGraph(current, previous);
  }
  const plan = {
    ...buildAffectedPlan(changes.files, {
      contracts,
      frontend,
      owners: options.owners,
      reason: options.reason,
    }),
    base: changes.base,
    deleted: changes.deleted,
    source: git(root, ["rev-parse", "HEAD"]).trim(),
    dirty: git(root, ["status", "--porcelain", "--untracked-files=normal"]).trim() !== "",
  };
  for (const contract of contracts) if (contract.capability) {
    for (const reference of [contract.capability.guide, ...contract.capability.invariants.map(item => item.reference)]) {
      try { await readFile(path.join(root, reference.split("#")[0])); }
      catch { throw Error(`Missing capability reference for ${contract.id}: ${reference}`); }
    }
  }
  const required = await loadRequirements(plan.files, root);
  const additional = selectRequirements(["manual"], [{ id: "manual-boundary", paths: ["manual"], browser: options.browser ?? [], native: options.native ?? [] }]);
  plan.requirements = {
    browser: [...new Set([...required.browser, ...additional.browser])].sort(),
    native: [...new Set([...required.native, ...additional.native])].sort(),
    owners: required.owners,
    ...(additional.browser.length || additional.native.length ? { addedReason: options.reason } : {}),
  };
  for (const spec of plan.requirements.browser) {
    try { await readFile(path.join(root, "frontend", spec)); }
    catch { throw Error(`Missing required browser spec: ${spec}`); }
  }
  for (const file of plan.node) await readFile(path.join(root, file));
  for (const file of plan.frontend)
    await readFile(path.join(root, "frontend", file));
  if (options.checks) plan.checks = buildStaticChecks(plan);
  return plan;
}

async function checked(run, program, args, options) {
  const result = await run(program, args, {
    ...options,
    timeoutMs: 2_147_483_647,
  });
  if (result.code !== 0 || result.timedOut || result.error || result.cancelled || result.cleanupUnverified)
    throw Object.assign(Error(`${program} failed: ${result.error || result.output?.slice(-6000) || "no output"}`), {
      kind: result.failureKind ?? (result.error && !result.timedOut ? "unavailable" : "failed"),
      reportMessage: `${program} failed (${result.timedOut ? "deadline" : `exit ${result.code ?? "unavailable"}`}); inspect the phase log.`,
    });
  return result;
}
export async function prepareRust(
  selections,
  { cwd = process.cwd(), run = runCommand, log = console.log } = {},
) {
  const prepared = [];
  for (const selection of selections) {
    const started = Date.now();
    const result = await checked(
      run,
      "cargo",
      [
        "test",
        "--locked",
        "-p",
        selection.package,
        "--lib",
        "--no-run",
        "--message-format=json",
      ],
      { cwd },
    );
    const output = requireCommandStdout(result);
    const executables = [];
    for (const line of output.split("\n")) {
      try {
        const item = JSON.parse(line);
        if (
          item.reason === "compiler-artifact" &&
          item.executable &&
          item.target?.kind?.includes("lib")
        )
          executables.push(item.executable);
      } catch {
        /* Cargo also emits text diagnostics. */
      }
    }
    if (executables.length !== 1)
      throw Error(
        `Missing or ambiguous library test executable for ${selection.package}`,
      );
    const executable = executables[0];
    const names = (text) =>
      [...text.matchAll(/^(.+): test\r?$/gmu)].map((match) => match[1]);
    const listed = names(requireCommandStdout(await checked(run, executable, ["--list"], { cwd })));
    const ignored = new Set(
      names(requireCommandStdout(await checked(run, executable, ["--list", "--ignored"], { cwd }))),
    );
    const runnable = listed.filter((name) => !ignored.has(name));
    for (const filter of selection.filters)
      if (!runnable.some((name) => name.includes(filter)))
        throw Error(
          `Selector matches zero nonignored ${selection.package} tests: ${filter}`,
        );
    const count = runnable.filter((name) =>
      selection.filters.some((filter) => name.includes(filter)),
    ).length;
    prepared.push({ ...selection, executable, count });
    log(
      `${selection.package}: compile/inventory ${((Date.now() - started) / 1000).toFixed(1)}s; ${count} selected tests`,
    );
  }
  return prepared;
}
export function validateFrontendReport(report, files, directory) {
  if (!Array.isArray(report.testResults))
    throw Error("Invalid frontend test report");
  let count = 0;
  for (const file of files) {
    const result = report.testResults.find(
      (item) =>
        path.resolve(directory, item.name) === path.resolve(directory, file),
    );
    const passed =
      result?.assertionResults?.filter(
        (assertion) => assertion.status === "passed",
      ).length ?? 0;
    if (!passed)
      throw Error(
        `Selected frontend file ran zero passing tests: ${file}`,
      );
    count += passed;
  }
  return count;
}
export async function executePlan(
  plan,
  { cwd = process.cwd(), run = runCommand, log = console.log, resultPrefix } = {},
) {
  if (plan.blockers.length)
    throw Error(`Validation blocked: ${plan.blockers.join("\n")}`);
  for (const [index, check] of (plan.checks ?? []).entries()) {
    log(`Static ${check.name}`);
    const result = await checked(run, process.execPath, [
      path.join(cwd, "scripts/validation/stage.mjs"), check.name,
      "--profile", "shared", ...(resultPrefix ? ["--result", `${resultPrefix}.${index}.json`] : []), ...(check.inputs ? ["--inputs", check.inputs] : []),
      ...(check.artifacts ?? []).flatMap(artifact => ["--artifact", artifact]),
      "--", ...check.command,
    ], { cwd: path.resolve(cwd, check.cwd) });
    log(result.output);
  }
  const prepared = await prepareRust(plan.rust, { cwd, run, log });
  async function execute(program, args, directory) {
    const started = Date.now();
    const result = await checked(run, program, args, { cwd: directory });
    log(result.output);
    log(`Test execution: ${((Date.now() - started) / 1000).toFixed(1)}s`);
    return result;
  }
  for (const file of plan.node) {
    const result = await execute(
      process.execPath,
      ["--test", "--test-reporter=tap", file],
      cwd,
    );
    const output = requireCommandStdout(result);
    const declared = new Set(
      [...output.matchAll(/^\s*# Subtest: (.+)$/gmu)]
        .map(match => match[1])
        .filter(name => path.resolve(cwd, name.replace(/\\([#\\])/gu, "$1")) !== path.resolve(cwd, file)),
    );
    // Aggregate TAP passes include Node's synthetic file wrapper. Require an
    // actual passing declared case; stdout can print a heading as a comment.
    const passed = [...output.matchAll(/^\s*ok [0-9]+ - (.+)$/gmu)].some(
      match => !/\s+#\s*(?:SKIP|TODO)\b/iu.test(match[1]) && declared.has(match[1]),
    );
    if (!passed || !/^# pass [1-9][0-9]*$/mu.test(output))
      throw Error(`Selected diagnostic ran zero passing Node tests: ${file}`);
  }
  for (const selection of prepared) {
    // Cargo applies repository configuration (including RUST_TEST_THREADS).
    // Executing the inventoried binary directly bypasses that environment.
    const result = await execute("cargo", [
      "test", "--locked", "-p", selection.package, "--lib", "--", ...selection.filters,
    ], cwd);
    if (rustTestCount(requireCommandStdout(result)) < selection.count)
      throw Error(
        `Expected ${selection.count} nonignored ${selection.package} tests; fewer executed`,
      );
  }
  if (plan.frontend.length) {
    const directory = await mkdtemp(
      path.join(tmpdir(), "mesh-affected-vitest-"),
    );
    const report = path.join(directory, "results.json");
    try {
      await execute(
        process.execPath,
        [
          path.join(cwd, "frontend/node_modules/vitest/vitest.mjs"),
          "run",
          ...plan.frontend,
          "--reporter=json",
          "--outputFile",
          report,
        ],
        path.join(cwd, "frontend"),
      );
      const count = validateFrontendReport(
        JSON.parse(await readFile(report, "utf8")),
        plan.frontend,
        path.join(cwd, "frontend"),
      );
      log(
        `Frontend: ${count} passing test cases in ${plan.frontend.length} selected files`,
      );
    } catch (error) {
      // Vitest's JSON reporter writes assertion failures only to this file.
      // Preserve their diagnostics before cleaning up the temporary report.
      try {
        const results = JSON.parse(await readFile(report, "utf8"));
        const failures = (results.testResults ?? []).flatMap(suite => [
          ...(suite.message ? [`${suite.name}: ${suite.message}`] : []),
          ...(suite.assertionResults ?? [])
            .filter(assertion => assertion.status === "failed")
            .map(assertion => `${suite.name}: ${assertion.fullName}\n${(assertion.failureMessages ?? []).join("\n")}`),
        ]);
        if (failures.length) error.message += `\n${failures.join("\n").slice(0, 6000)}`;
      } catch {
        // Missing/malformed reports must not hide the original execution error.
      }
      throw error;
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }
}

// The phase handoff also protects dirty development edits, which cannot create
// reusable receipts. A different plan or changed bytes require fresh static work.
export async function executionFingerprint(root, plan) {
  const hash = createHash("sha256").update(JSON.stringify(plan));
  for (const file of [...plan.files].sort()) {
    try {
      const filename = path.join(root, file);
      const info = await lstat(filename);
      const bytes = info.isSymbolicLink() ? await readlink(filename) : await readFile(filename);
      hash.update(JSON.stringify([file, info.mode, createHash("sha256").update(bytes).digest("hex")]));
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
      hash.update(JSON.stringify([file, "absent"]));
    }
  }
  hash.update(JSON.stringify(await frontendEnvironmentIdentity(root)));
  return hash.digest("hex");
}
function printPlan(plan) {
  console.log(
    `Development validation against ${plan.base.slice(0, 12)} (${plan.files.length} changed files)`,
  );
  for (const owner of plan.owners)
    console.log(`Owner ${owner.id}: ${owner.reasons.join("; ")}`);
  for (const selection of plan.rust)
    console.log(`Rust ${selection.package}: ${selection.filters.join(", ")}`);
  for (const file of plan.node) console.log(`Node: ${file}`);
  for (const file of plan.frontend)
    console.log(`Frontend ${file}: ${plan.frontendReasons[file].join("; ")}`);
  for (const check of plan.checks ?? [])
    console.log(`Static ${check.name}: ${check.command.join(" ")}`);
  for (const file of plan.acknowledged)
    console.log(`Explicitly reviewed unmapped file: ${file}`);
  for (const owner of plan.owners) if (owner.capability) {
    console.log(`Capability ${owner.id}: ${owner.capability.summary} (${owner.capability.guide})`);
    for (const invariant of owner.capability.invariants) console.log(`Invariant ${invariant.name}: ${invariant.reference}`);
  }
  for (const spec of plan.requirements?.browser ?? []) console.log(`Pending browser evidence: ${spec}`);
  for (const platform of plan.requirements?.native ?? []) console.log(`Pending native evidence: ${platform}`);
  for (const note of plan.escalation) console.log(`Escalation: ${note}`);
  for (const blocker of plan.blockers) console.log(`BLOCKED: ${blocker}`);
  console.log(plan.scope);
}
async function reportFilename(root, requested) {
  let filename = requested ? path.resolve(root, requested) : path.join(root, "target/validation/feedback", `${Date.now()}-${randomUUID()}.json`);
  // Git canonicalizes /var -> /private/var on macOS. Resolve existing parents
  // before checking containment, also preventing an output-directory symlink escape.
  let parent = path.dirname(filename);
  const suffix = [path.basename(filename)];
  while (true) {
    try { filename = path.join(await realpath(parent), ...suffix); break; }
    catch (error) {
      if (error.code !== "ENOENT" || parent === path.dirname(parent)) throw error;
      suffix.unshift(path.basename(parent)); parent = path.dirname(parent);
    }
  }
  if (!/^(?:target|tmp)[/\\]/u.test(path.relative(root, filename)) || !filename.endsWith(".json"))
    throw Error("Feedback reports must be JSON files under this worktree's ignored target/ or tmp/");
  const ignored = spawnSync("git", ["check-ignore", "--quiet", "--", filename], { cwd: root });
  if (ignored.status !== 0) throw Error("Feedback report destination must be ignored by Git");
  return filename;
}
async function terminalFeedback(filename, plan, options, started) {
  const report = finishFeedback(createFeedback(plan, { checks: options.checks, started }));
  await writeFeedback(filename, report);
  console.log(`Development feedback: ${report.status}; candidate acceptance pending. Report: ${filename}`);
}
async function main(args) {
  const options = parseOptions(args);
  const invocationStarted = Date.now();
  const root = git(process.cwd(), ["rev-parse", "--show-toplevel"]).trim();
  const childProgress = process.env.MESH_VALIDATION_AFFECTED_PROGRESS;
  const reportFile = options.run && !process.env.MESH_VALIDATION_AFFECTED_CHILD ? await reportFilename(root, options.report) : undefined;
  let plan;
  try { plan = await createPlan(root, options); }
  catch (error) {
    if (childProgress) await observeCommands(childProgress, {
      phase: process.env.MESH_VALIDATION_AFFECTED_PHASE || "all",
      fingerprint: process.env.MESH_VALIDATION_AFFECTED_INPUTS,
    }).fail(Object.assign(Error(`Validation plan changed or became invalid: ${error.message}`), { kind: "invalidated" }));
    if (reportFile) await terminalFeedback(reportFile, {
      source: git(root, ["rev-parse", "HEAD"]).trim(), dirty: git(root, ["status", "--porcelain", "--untracked-files=normal"]).trim() !== "", owners: [], rust: [], frontend: [], node: [],
      blockers: [error.message], escalation: [],
    }, options, invocationStarted);
    throw error;
  }
  if (options.json) console.log(JSON.stringify(plan, null, 2));
  else printPlan(plan);
  if (plan.blockers.length) {
    if (childProgress) await observeCommands(childProgress, {
      phase: process.env.MESH_VALIDATION_AFFECTED_PHASE || "all",
      fingerprint: process.env.MESH_VALIDATION_AFFECTED_INPUTS,
    }).fail(Object.assign(Error("Validation plan became blocked; preview the changed inputs."), { kind: "invalidated" }));
    if (reportFile) await terminalFeedback(reportFile, plan, options, invocationStarted);
    process.exitCode = 2;
    return;
  }
  const expectedResource = process.env.MESH_VALIDATION_AFFECTED_CHILD;
  const phase = process.env.MESH_VALIDATION_AFFECTED_PHASE || "all";
  if (!["all", "static", "tests"].includes(phase)) throw Error("Unknown validation phase");
  const resource = plan.rust.length || plan.checks?.some(check => check.name === "frontend-build")
    ? "heavy"
    : plan.frontend.length
      ? "focused-frontend"
      : "focused-node";
  const child = !!expectedResource;
  if (!options.run && !child) return;
  if (!child && !plan.rust.length && !plan.node.length && !plan.frontend.length && !plan.checks?.length) {
    console.log("No tests selected for the current change set.");
    if (reportFile) await terminalFeedback(reportFile, plan, options, invocationStarted);
    return;
  }
  if (child) {
    const expectedInputs = process.env.MESH_VALIDATION_AFFECTED_INPUTS;
    const progressFile = process.env.MESH_VALIDATION_AFFECTED_PROGRESS;
    const observer = progressFile ? observeCommands(progressFile, { phase, fingerprint: expectedInputs }) : undefined;
    try {
      if ((phase === "static" ? "focused-static" : resource) !== expectedResource)
        throw Object.assign(Error("Validation resource class changed while queued; preview and run the updated plan."), { kind: "invalidated" });
      if ((phase !== "all" || expectedInputs) && (!expectedInputs || await executionFingerprint(root, plan) !== expectedInputs))
        throw Object.assign(Error("Validation inputs changed between phases; preview and rerun the updated plan."), { kind: "invalidated" });
      delete process.env.MESH_VALIDATION_AFFECTED_CHILD;
      delete process.env.MESH_VALIDATION_AFFECTED_PHASE;
      delete process.env.MESH_VALIDATION_AFFECTED_INPUTS;
      delete process.env.MESH_VALIDATION_AFFECTED_PROGRESS;
      await executePlan(phase === "all" ? plan : phasePlan(plan, phase), {
        cwd: root, ...(observer ? { run: observer.wrap(runCommand), resultPrefix: progressFile } : {}),
      });
      if ((phase !== "all" || expectedInputs) && await executionFingerprint(root, await createPlan(root, options)) !== expectedInputs)
        throw Object.assign(Error("Validation inputs changed during the phase; preview and rerun the updated plan."), { kind: "invalidated" });
    } catch (error) { await observer?.fail(error); throw error; }
    return;
  }
  await mkdir(path.join(root, "target/validation"), { recursive: true });
  // The child recomputes the plan inside the existing cross-worktree resource lock.
  const forwarded = [];
  for (let index = 0; index < args.length; index++) {
    if (args[index] === "--report") { index++; continue; }
    if (!["--run", "--json"].includes(args[index])) forwarded.push(args[index]);
  }
  const controller = new AbortController();
  const fingerprint = await executionFingerprint(root, plan);
  const report = createFeedback(plan, { checks: options.checks, fingerprint, started: invocationStarted });
  const runId = randomUUID();
  const progressFile = name => path.join(root, "target/validation/feedback", `${runId}.${name}.progress.json`);
  let currentPhase;
  async function collect(name, record, error) {
    let progress;
    try { progress = JSON.parse(await readFile(progressFile(name), "utf8")); }
    catch (failure) { if (failure.code !== "ENOENT") throw failure; }
    if (progress && (progress.phase !== (options.checks ? name : "all") || progress.fingerprint !== fingerprint))
      throw Object.assign(Error("Phase progress belongs to different validation inputs"), { kind: "invalidated" });
    completePhase(report, name, { record, progress, error, aborted: controller.signal.aborted });
    await writeFeedback(reportFile, report);
  }
  await writeFeedback(reportFile, report);
  // One owner bounds queueing and cleans up the complete managed process group.
  const deadline = setTimeout(() => controller.abort(), 1_800_000);
  const stop = () => controller.abort();
  process.on("SIGINT", stop); process.on("SIGTERM", stop);
  try {
    if (options.checks && phasePlan(plan, "static").checks.length) {
      currentPhase = "static";
      const staticPhase = report.phases.find(item => item.name === currentPhase);
      staticPhase.status = "running"; staticPhase.started = Date.now();
      await writeFeedback(reportFile, report);
      const fast = await runStage({
        root, cwd: root, name: "affected-static", resource: "focused-static", profile: "shared",
        context: { base: plan.base, plan, phase: "static", fingerprint },
        env: { ...process.env, MESH_VALIDATION_AFFECTED_CHILD: "focused-static", MESH_VALIDATION_AFFECTED_PHASE: "static", MESH_VALIDATION_AFFECTED_INPUTS: fingerprint, MESH_VALIDATION_AFFECTED_PROGRESS: progressFile("static") },
        signal: controller.signal,
        command: [process.execPath, fileURLToPath(import.meta.url), ...forwarded],
      });
      await collect("static", fast);
      if (fast.code !== 0) { process.exitCode = 1; return; }
    }
    const artifacts = [...new Set((plan.checks ?? []).flatMap(check =>
      (check.artifacts ?? []).map(artifact => path.join(check.cwd, artifact))))];
    const childCommand = [process.execPath, fileURLToPath(import.meta.url), ...forwarded];
    // Retention must precede the shared build resource, but must not delay cheap checks.
    const command = options.managed ? [
      "python3", path.join(root, "scripts/build-retention.py"), "run", "--",
      process.execPath, path.join(root, "scripts/validation/stage.mjs"), "affected-heavy",
      "--profile", "shared", "--resource", resource,
      "--context", fingerprint ?? JSON.stringify(plan),
      ...artifacts.flatMap(artifact => ["--artifact", artifact]),
      "--", ...childCommand,
    ] : childCommand;
    currentPhase = "tests";
    const testPhase = report.phases.find(item => item.name === currentPhase);
    if (testPhase) { testPhase.status = "running"; testPhase.started = Date.now(); }
    await writeFeedback(reportFile, report);
    const result = await runStage({
      root,
      cwd: root,
      name: "affected-tests",
      resource: options.managed ? undefined : resource,
      artifacts,
      profile: "shared",
      context: { base: plan.base, plan, phase: options.checks ? "tests" : "all", fingerprint },
      env: { ...process.env, MESH_VALIDATION_AFFECTED_CHILD: resource,
        MESH_VALIDATION_AFFECTED_PHASE: options.checks ? "tests" : "all", MESH_VALIDATION_AFFECTED_INPUTS: fingerprint, MESH_VALIDATION_AFFECTED_PROGRESS: progressFile("tests") },
      signal: controller.signal,
      command,
    });
    await collect("tests", result);
    process.exitCode = result.code === 0 ? 0 : 1;
  } catch (error) {
    if (currentPhase) completePhase(report, currentPhase, {
      error, aborted: controller.signal.aborted,
      progress: { commands: report.phases.find(item => item.name === currentPhase)?.commands ?? [] },
    });
    throw error;
  } finally {
    clearTimeout(deadline);
    process.off("SIGINT", stop); process.off("SIGTERM", stop);
    finishFeedback(report);
    await writeFeedback(reportFile, report);
    console.log(`Development feedback: ${report.status}; candidate acceptance pending. Report: ${reportFile}`);
  }
}
if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
)
  main(process.argv.slice(2)).catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
