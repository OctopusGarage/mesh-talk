import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createFeedback, completePhase, finishFeedback, observeCommands } from "../validation/feedback.mjs";

const plan = () => ({ source: "a".repeat(40), base: "b".repeat(40), dirty: true,
  owners: [], blockers: [], escalation: ["Review dynamic IPC consumers"],
  checks: [{ name: "lint", resource: undefined }, { name: "build", resource: "heavy" }],
  rust: [], node: ["scripts/a.test.mjs"], frontend: [],
  requirements: { browser: ["e2e/ui.spec.ts"], native: ["win32"], owners: ["visual"] } });

test("successful local phases leave boundary review and candidate acceptance pending", () => {
  const report = createFeedback(plan(), { checks: true });
  completePhase(report, "static", { record: { code: 0, reused: false } });
  completePhase(report, "tests", { record: { code: 0, reused: false } });
  finishFeedback(report);
  assert.equal(report.status, "passed");
  assert.equal(report.candidateReady, false);
  assert(report.remaining.some(item => item.kind === "browser" && item.status === "not-run"));
  assert(report.remaining.some(item => item.kind === "native" && item.status === "not-run"));
  assert(report.remaining.some(item => item.kind === "review" && item.status === "needs-review"));
  assert(report.remaining.some(item => item.kind === "candidate" && item.status === "not-run"));
});

test("phase failure preserves pending work, cancellation and launch unavailability stay distinct", () => {
  for (const [record, failure, status] of [
    [{ code: 1 }, { kind: "failed", message: "assertion failed" }, "failed"],
    [{ code: 1 }, { kind: "unavailable", message: "missing executable" }, "unavailable"],
    [{ code: 130 }, undefined, "cancelled"],
    [{ code: 1 }, { kind: "invalidated", message: "inputs changed" }, "invalidated"],
  ]) {
    const report = createFeedback(plan(), { checks: true });
    completePhase(report, "static", { record, progress: { failure, commands: [] } });
    finishFeedback(report);
    assert.equal(report.status, status);
    assert.equal(report.phases[1].status, "not-run");
    assert.equal(report.candidateReady, false);
  }
});

test("empty selection differs from passed tests and blocked planning never authorizes execution", () => {
  const empty = createFeedback({ ...plan(), node: [], checks: [], requirements: {} }, { checks: true });
  finishFeedback(empty);
  assert.equal(empty.status, "no-checks");
  const blocked = createFeedback({ ...plan(), blockers: ["unmapped source"] }, { checks: true });
  finishFeedback(blocked);
  assert.equal(blocked.status, "blocked");
});

test("receipt reuse records scoped provenance without inventing command executions", () => {
  const report = createFeedback(plan(), { checks: false });
  completePhase(report, "tests", { record: { code: 0, reused: true, receipt: "proof.json", source: "a".repeat(40) } });
  finishFeedback(report);
  assert.equal(report.phases[0].reused, true);
  assert.deepEqual(report.phases[0].commands, []);
  assert.equal(report.mode, "tests-only");
  assert.equal(report.candidateReady, false);
});

test("command progress records real outcomes but never command output or environment", async t => {
  const root = await mkdtemp(path.join(tmpdir(), "mesh-feedback-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const filename = path.join(root, "progress.json");
  const observer = observeCommands(filename, { phase: "tests", fingerprint: "identity" });
  const run = observer.wrap(async () => ({ code: 1, output: "private output", error: undefined }));
  await run("tool", ["test"], { cwd: root, env: { TOKEN: "private environment" } });
  const body = await readFile(filename, "utf8");
  const data = JSON.parse(body);
  assert.equal(data.commands[0].status, "failed");
  assert.equal(data.fingerprint, "identity");
  assert(!body.includes("private"));
});

test("structured command-launch errors and nested stage outcomes stay unavailable", async t => {
  const root = await mkdtemp(path.join(tmpdir(), "mesh-feedback-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const filename = path.join(root, "progress.json");
  const observer = observeCommands(filename, { phase: "static", fingerprint: "identity" });
  const result = await observer.wrap(async () => ({ code: null, output: "", error: "spawn ENOENT" }))("missing", [], { cwd: root });
  assert.equal(result.failureKind, "unavailable");
  await observer.fail(Object.assign(Error("missing tool"), { kind: "unavailable" }));
  assert.equal(JSON.parse(await readFile(filename, "utf8")).failure.kind, "unavailable");
});

test("supervisor invalidation preserves its reason even without child progress", () => {
  const report = createFeedback(plan(), { checks: false });
  completePhase(report, "tests", { record: { code: 1, error: "Source changed during validation", failureKind: "invalidated" } });
  finishFeedback(report);
  assert.equal(report.status, "invalidated");
  assert.equal(report.phases[0].failure.message, "Source changed during validation");
});

test("a missing tool beneath a real static stage is unavailable, not a failed test assertion", async t => {
  const { spawnSync } = await import("node:child_process");
  const { writeFile } = await import("node:fs/promises");
  const { runCommand } = await import("../pr-platform/guest.mjs");
  const { executePlan } = await import("../validation/changed.mjs");
  const root = await mkdtemp(path.join(tmpdir(), "mesh-feedback-tool-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const git = args => assert.equal(spawnSync("git", args, { cwd: root, encoding: "utf8" }).status, 0);
  git(["init", "-q"]);
  await writeFile(path.join(root, ".gitignore"), "target/\n");
  git(["add", "."]);
  git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "fixture"]);
  const filename = path.join(root, "target/progress.json");
  const observer = observeCommands(filename, { phase: "static", fingerprint: "identity" });
  const unavailable = path.join(root, "missing-executable");
  const fixturePlan = { blockers: [], checks: [{ name: "missing-tool-fixture", cwd: root, command: [unavailable] }], rust: [], node: [], frontend: [] };
  // The production stage path comes from the actual repository. Only the
  // invoked command, Git identity and evidence live in the disposable fixture.
  const cwd = fileURLToPath(new URL("../../", import.meta.url));
  const run = observer.wrap((program, args, options) => runCommand(program, args, { ...options,
    env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined } }));
  let failure;
  try { await executePlan(fixturePlan, { cwd, run, resultPrefix: filename, log: () => {} }); }
  catch (error) { failure = error; }
  assert.equal(failure?.kind, "unavailable", await readFile(filename, "utf8"));
  const progress = JSON.parse(await readFile(filename, "utf8"));
  assert.equal(progress.commands[0].status, "unavailable");
  assert.equal(progress.commands[0].stage.code, 1);
});

test("feedback timing separates first process result, actionable issue and full completion", async () => {
  const { feedbackTiming } = await import("../validation/feedback.mjs");
  const report = { started: 1000, finished: 11000, status: "failed", phases: [
    { name: "static", started: 2000, finished: 10000, status: "failed", commands: [
      { started: 3000, finished: 4000, status: "passed" },
      { started: 5000, finished: 9000, status: "unavailable" },
    ] },
  ] };
  const timing = feedbackTiming(report);
  assert.equal(timing.firstCommandSeconds, 3);
  assert.deepEqual(timing.firstIssue, { seconds: 8, kind: "unavailable", phase: "static", origin: "command" });
  assert.equal(timing.completionSeconds, 10);
});

test("phase validation errors remain measurable after a process exited successfully", async () => {
  const { feedbackTiming } = await import("../validation/feedback.mjs");
  const timing = feedbackTiming({ started: 1000, finished: 5000, status: "failed", phases: [
    { name: "tests", finished: 4500, status: "failed", commands: [{ started: 2000, finished: 3000, status: "passed" }] },
  ] });
  assert.equal(timing.firstCommandSeconds, 2);
  assert.deepEqual(timing.firstIssue, { seconds: 3.5, kind: "failed", phase: "tests", origin: "phase" });
});

test("reused phases never fabricate fresh commands and cancellation is not a test failure", async () => {
  const { feedbackTiming } = await import("../validation/feedback.mjs");
  const timing = feedbackTiming({ started: 1000, finished: 5000, status: "cancelled", phases: [
    { name: "static", status: "passed", reused: true, commands: [{ started: 2000, finished: 3000, status: "passed" }] },
    { name: "tests", status: "cancelled", finished: 4000, commands: [{ started: 3500, status: "interrupted" }] },
  ] });
  assert.equal(timing.firstCommandSeconds, null);
  assert.equal(timing.firstIssue, null);
  assert.equal(timing.reusedPhases, 1);
});

test("missing, backwards and out-of-envelope observations stay unknown", async () => {
  const { feedbackTiming } = await import("../validation/feedback.mjs");
  const report = { started: 1000, finished: 5000, status: "failed", phases: [{ name: "tests", status: "failed", commands: [
    { started: 3000, finished: 2000, status: "failed" },
    { started: 4000, finished: 6000, status: "failed" },
    { started: 0, finished: 2000, status: "failed" },
    { started: "1000", finished: 3000, status: "passed" },
  ] }] };
  assert.equal(feedbackTiming(report).firstCommandSeconds, null);
  assert.equal(feedbackTiming(report).firstIssue, null);
  assert.equal(feedbackTiming({ ...report, finished: 500 }).completionSeconds, null);
});

test("an unfinished run can expose an issue without claiming completion or a regression rate", async () => {
  const { feedbackTiming } = await import("../validation/feedback.mjs");
  const timing = feedbackTiming({ started: 1000, status: "running", phases: [
    { name: "tests", status: "running", commands: [{ started: 2000, finished: 3000, status: "failed" }] },
  ] });
  assert.equal(timing.firstIssue.kind, "failed");
  assert.equal(timing.completionSeconds, null);
});

test("timing tolerates incomplete progress and labels nested reuse explicitly", async () => {
  const { feedbackTiming } = await import("../validation/feedback.mjs");
  const timing = feedbackTiming({ started: 1000, finished: null, status: "running", phases: [null,
    { name: "static", status: "running", finished: null, commands: [null, { started: 2000, finished: 3000, status: "passed", stage: { reused: true } }] },
    { name: "tests", commands: {} },
  ] });
  assert.equal(timing.firstCommandSeconds, 2);
  assert.equal(timing.firstCommandReused, true);
  assert.equal(timing.reusedCommands, 1);
  assert.equal(timing.completionSeconds, null);
});

test("cancelling a run cannot erase an earlier observed failure", async () => {
  const { feedbackTiming } = await import("../validation/feedback.mjs");
  const timing = feedbackTiming({ started: 1000, finished: 5000, status: "cancelled", phases: [
    { name: "tests", status: "cancelled", finished: 4500, commands: [{ started: 2000, finished: 3000, status: "failed" }] },
  ] });
  assert.equal(timing.firstIssue.kind, "failed");
  assert.equal(timing.firstIssue.seconds, 2);
});

test("cancelled phase retains the observation time of an earlier validation issue", async () => {
  const { feedbackTiming } = await import("../validation/feedback.mjs");
  const timing = feedbackTiming({ started: 1000, finished: 5000, status: "cancelled", phases: [
    { name: "tests", started: 1500, status: "cancelled", finished: 4500, commands: [], failure: { kind: "invalidated", observedAt: 3000, message: "source changed" } },
  ] });
  assert.deepEqual(timing.firstIssue, { seconds: 2, kind: "invalidated", phase: "tests", origin: "phase" });
  assert.equal(timing.completionSeconds, 4);
});
