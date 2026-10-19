/** Development observations only; this report never certifies candidate acceptance. */
import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";
import { phasePlan } from "./checks.mjs";

export async function writeFeedback(filename, report) {
  await mkdir(path.dirname(filename), { recursive: true });
  const temporary = `${filename}.${randomUUID()}.tmp`;
  await writeFile(temporary, `${JSON.stringify(report, null, 2)}\n`);
  await rename(temporary, filename);
}
export function failureKind(error) {
  if (["failed", "unavailable", "invalidated"].includes(error?.kind)) return error.kind;
  if (["ENOENT", "EACCES"].includes(error?.code)) return "unavailable";
  // These are supervisor errors, not text extracted from test output.
  if (/^(Source|Frontend environment|Stage inputs) changed/u.test(error?.message ?? "")) return "invalidated";
  return "failed";
}
export function createFeedback(plan, { checks = false, fingerprint, started } = {}) {
  const phases = checks ? ["static", "tests"] : ["tests"];
  return {
    schema: 1, kind: "development-feedback", mode: checks ? "checks-and-tests" : "tests-only",
    candidateReady: false, status: "running", source: plan.source, base: plan.base,
    dirty: plan.dirty, fingerprint, started: started ?? Date.now(), timingOrigin: started === undefined ? "report-created" : "invocation", platform: process.platform,
    capabilities: plan.owners.filter(owner => owner.capability), blockers: plan.blockers,
    phases: phases.flatMap(name => {
      const slice = checks ? phasePlan(plan, name) : plan;
      const obligations = { checks: (slice.checks ?? []).map(check => check.name), rust: slice.rust, node: slice.node, frontend: slice.frontend };
      return Object.values(obligations).some(items => items?.length) ? [{ name, status: "not-run", obligations, commands: [] }] : [];
    }),
    remaining: [
      ...(plan.requirements?.browser ?? []).map(spec => ({ kind: "browser", selection: spec, status: "not-run" })),
      ...(plan.requirements?.native ?? []).map(platform => ({ kind: "native", selection: platform, status: "not-run" })),
      ...plan.escalation.map(instruction => ({ kind: "review", status: "needs-review", instruction })),
      { kind: "candidate", status: "not-run", instruction: "Commit the candidate and run ./scripts/prepare-pr.sh with affected Rust selectors, required browser specs and native evidence. Full health and protected CI remain required." },
    ],
    limitations: "Observed development scope only. Browser/native requirements are pending, not executed by this runner. Command success alone does not prove test selection or assertions; the phase must also pass its inventory, nonempty-test and unchanged-input checks. This report is not publication evidence.",
  };
}
export function completePhase(report, name, { record, progress, error, aborted = false } = {}) {
  const phase = report.phases.find(item => item.name === name);
  if (!phase) return;
  phase.status = aborted || record?.code === 130 ? "cancelled" : error ? failureKind(error) :
    record?.code === 0 ? "passed" : progress?.failure?.kind ?? record?.failureKind ?? "failed";
  phase.commands = record?.reused ? [] : (progress?.commands ?? []);
  phase.reused = record?.reused ?? false;
  phase.finished = Date.now();
  phase.evidence = record ? { source: record.source, key: record.key, logPath: record.logPath, receipt: record.receipt, queuedMs: record.queuedMs, executionMs: record.executionMs, code: record.code } : undefined;
  phase.failure = error ? { kind: failureKind(error), message: error.reportMessage ?? error.message, observedAt: phase.finished } : progress?.failure ?? (record?.error ? { kind: record.failureKind ?? "failed", message: record.error, observedAt: phase.finished } : undefined);
  if (phase.status !== "passed")
    for (const command of phase.commands) if (command.status === "running") command.status = "interrupted";
}
export function finishFeedback(report) {
  report.finished = Date.now();
  report.status = report.blockers.length ? "blocked" :
    report.phases.find(phase => !["passed", "not-run"].includes(phase.status))?.status ??
    (report.phases.some(phase => phase.status === "not-run") ? "not-run" : report.phases.length ? "passed" : "no-checks");
  report.next = [
    ...report.blockers.map(instruction => ({ kind: "planning", instruction })),
    ...report.phases.filter(phase => phase.status !== "passed").map(phase => ({ kind: "development", phase: phase.name,
      instruction: phase.status === "unavailable" ? "Restore the required tool/environment and rerun this plan." : phase.status === "invalidated" ? "Preview the changed source and rerun validation." : "Inspect phase evidence, resolve the failure or pending work, and rerun this plan." })),
    ...report.remaining,
  ];
  report.timing = feedbackTiming(report);
  return report;
}

/** Progress is private to one run/phase; output and environment values are omitted. */
export function observeCommands(filename, identity) {
  const progress = { schema: 1, ...identity, commands: [] };
  const save = () => writeFeedback(filename, progress);
  return {
    wrap: run => async (program, args, options) => {
      const command = { program, args, cwd: options?.cwd, status: "running", started: Date.now() };
      progress.commands.push(command);
      await save();
      let result;
      try { result = await run(program, args, options); }
      catch (error) { result = { code: null, error: error.message, failureKind: failureKind(error) }; }
      // A static stage can report a launch failure beneath the Node wrapper.
      const resultIndex = args.indexOf("--result");
      if (resultIndex >= 0) {
        try {
          const nested = JSON.parse(await readFile(args[resultIndex + 1], "utf8"));
          if (nested.code === result.code && nested.failureKind) result.failureKind = nested.failureKind;
          command.stage = { name: nested.name, code: nested.code, reused: nested.reused, logPath: nested.logPath };
        } catch { /* A killed/unstarted wrapper has no completed outcome. */ }
      }
      if (result.error && !result.timedOut && !result.failureKind) result.failureKind = "unavailable";
      command.status = result.code === 0 && !result.error && !result.timedOut ? "passed" : result.failureKind ?? "failed";
      command.code = result.code; command.finished = Date.now(); command.timedOut = !!result.timedOut;
      await save();
      return result;
    },
    fail: async error => { progress.failure = { kind: failureKind(error), message: error.reportMessage ?? error.message, observedAt: Date.now() }; await save(); },
  };
}


/** Observed wall-clock events only; a process result is not business acceptance. */
export function feedbackTiming(report) {
  const valid = value => typeof value === "number" && Number.isFinite(value) && value >= 0;
  const start = report.started;
  const end = report.finished;
  const hasEnd = end !== undefined && end !== null;
  const envelope = valid(start) && (!hasEnd || valid(end) && end >= start);
  const within = time => envelope && valid(time) && time >= start && (!hasEnd || time <= end);
  const seconds = time => (time - start) / 1000;
  const results = [], issues = [];
  let reusedPhases = 0;
  for (const phase of Array.isArray(report.phases) ? report.phases : []) {
    if (!phase || typeof phase !== "object" || Array.isArray(phase)) continue;
    if (phase.reused === true) { reusedPhases++; continue; }
    const hasPhaseStart = phase.started !== undefined && phase.started !== null;
    const hasPhaseEnd = phase.finished !== undefined && phase.finished !== null;
    const phaseValid = (!hasPhaseStart || within(phase.started)) &&
      (!hasPhaseEnd || within(phase.finished)) &&
      !(valid(phase.started) && valid(phase.finished) && phase.finished < phase.started);
    if (!phaseValid) continue;
    const inPhase = time => within(time) && (!hasPhaseStart || time >= phase.started) &&
      (!hasPhaseEnd || time <= phase.finished);
    for (const command of Array.isArray(phase.commands) ? phase.commands : []) {
      if (!command || typeof command !== "object" || !inPhase(command.started) || !inPhase(command.finished) || command.finished < command.started ||
          !["passed", "failed", "unavailable", "invalidated"].includes(command.status)) continue;
      const result = { seconds: seconds(command.finished), reused: command.stage?.reused === true };
      results.push(result);
      if (command.status !== "passed") issues.push({ seconds: result.seconds, kind: command.status, phase: phase.name, origin: "command" });
    }
    if (["failed", "unavailable", "invalidated"].includes(phase.status) && inPhase(phase.finished))
      issues.push({ seconds: seconds(phase.finished), kind: phase.status, phase: phase.name, origin: "phase" });
    if (["failed", "unavailable", "invalidated"].includes(phase.failure?.kind) && inPhase(phase.failure.observedAt))
      issues.push({ seconds: seconds(phase.failure.observedAt), kind: phase.failure.kind, phase: phase.name, origin: "phase" });
  }
  if (report.status === "blocked" && within(end))
    issues.push({ seconds: seconds(end), kind: "blocked", phase: null, origin: "report" });
  results.sort((a, b) => a.seconds - b.seconds);
  issues.sort((a, b) => a.seconds - b.seconds);
  return {
    firstCommandSeconds: results[0]?.seconds ?? null,
    firstCommandReused: results[0]?.reused ?? null,
    firstIssue: issues[0] ?? null,
    completionSeconds: ["passed", "failed", "unavailable", "invalidated", "cancelled", "blocked", "no-checks", "not-run"].includes(report.status) && hasEnd && within(end) ? seconds(end) : null,
    observedCommands: results.length, reusedCommands: results.filter(result => result.reused).length, reusedPhases,
  };
}
