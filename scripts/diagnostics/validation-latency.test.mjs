import assert from "node:assert/strict";
import test from "node:test";
import { summarize, quantile, formatReport, parseArgs } from "../validation/latency.mjs";

const at = seconds => new Date(Date.UTC(2026, 0, 1) + seconds * 1000).toISOString();
const job = (extra = {}) => ({ id: 1, name: "Tests (ubuntu-latest)", labels: ["ubuntu-latest"], status: "completed", conclusion: "success", created_at: at(0), started_at: at(360), completed_at: at(390), steps: [
  { name: "Setup Rust", status: "completed", conclusion: "success", started_at: at(360), completed_at: at(365) },
  { name: "Compile workspace", status: "completed", conclusion: "success", started_at: at(365), completed_at: at(380) },
  { name: "Run tests", status: "completed", conclusion: "success", started_at: at(380), completed_at: at(390) },
], ...extra });
const run = jobs => ({ id: 1, status: "completed", conclusion: "success", created_at: at(0), jobs });

test("separates six minutes queue from thirty seconds execution and categorizes steps", () => {
  const result = summarize({ runs: [run([job()])] });
  assert.equal(result.jobs.completed.queue.p50Seconds, 360);
  assert.equal(result.jobs.completed.run.p50Seconds, 30);
  assert.equal(result.jobs.completed.runnerMinutes, 0.5);
  assert.equal(result.steps.setup.totalSeconds, 5);
  assert.equal(result.steps.compilation.totalSeconds, 15);
  assert.equal(result.steps.testing.totalSeconds, 10);
  assert.equal(result.byJob["Tests (ubuntu-latest)"].completed.queue.p95Seconds, 360);
});

test("reports elapsed critical path separately from parallel runner minutes and splits OS", () => {
  const result = summarize({ runs: [run([job(), job({ id: 2, labels: ["windows-latest"], started_at: at(370), completed_at: at(430) })])] });
  assert.equal(result.jobs.completed.runnerMinutes, 1.5);
  assert.equal(result.runs[0].elapsedSeconds, 430);
  assert.equal(result.runs[0].executionSpanSeconds, 70);
  assert.equal(result.byOs.linux.completed.run.count, 1);
  assert.equal(result.byOs.windows.completed.run.p50Seconds, 60);
});

test("pending placeholder timestamps, skipped and malformed intervals never inflate completed samples", () => {
  const result = summarize({ runs: [run([
    job({ status: "queued", conclusion: null }),
    job({ conclusion: "skipped" }),
    job({ started_at: at(400) }),
    job({ started_at: at(390), completed_at: at(390) }),
    job({ conclusion: "cancelled" }),
  ])] });
  assert.equal(result.jobs.completed.count, 2);
  assert.equal(result.jobs.completed.run.count, 1);
  assert.equal(result.jobs.completed.run.p50Seconds, 0);
  assert.equal(result.jobs.unfinished.run.count, 0);
  assert.equal(result.jobs.skipped.run.count, 0);
  assert.equal(result.jobs.cancelled.run.count, 1);
  assert.equal(result.runs[0].elapsedSeconds, null);
});

test("nearest rank quantiles preserve small sample tails and handle empty samples", () => {
  assert.equal(quantile([], 0.95), null);
  assert.equal(quantile([30, 10, 20, 40], 0.5), 20);
  assert.equal(quantile([30, 10, 20, 40], 0.95), 40);
});

test("local receipts show provenance and stale or unexecuted states without invented timings", () => {
  const result = summarize({ local: [
    { name: "unit", source: "old", code: 0, elapsedMs: 390000, executionMs: 30000, queuedMs: 360000, finished: 1 },
    { name: "planned", source: "new" },
    { name: "reused", source: "new", code: 0, executionMs: 12000, queuedMs: 0, reused: true },
  ] }, { revision: "new", dirty: false });
  assert.equal(result.local.records[0].freshness, "stale");
  assert.equal(result.local.records[0].sourceSha, "old");
  assert.equal(result.local.records[1].state, "unexecuted");
  assert.equal(result.local.records[2].state, "reused");
  assert.equal(result.local.executed.run.count, 1);
  assert.equal(result.local.executed.queue.p50Seconds, 360);
  assert.match(formatReport(result), /nearest-rank/i);
});

test("input flags reject ambiguous or malformed collection requests", () => {
  assert.throws(() => parseArgs(["--limit", "0"]), /limit/);
  assert.throws(() => parseArgs(["--repo", "x/y", "--input", "file"]), /Choose/);
  assert.throws(() => parseArgs(["--repo", "bad"]), /owner\/repo/);
  assert.equal(parseArgs(["--input", "a.json", "--json"]).json, true);
});


test("unfinished workflow envelopes remain unknown even when currently listed jobs finished", () => {
  const result = summarize({ runs: [{ ...run([job()]), status: "in_progress", conclusion: null }] });
  assert.equal(result.runs[0].elapsedSeconds, null);
});

test("local guest reports inherit source provenance, and dirty source is explicit", () => {
  const result = summarize({ local: [{ sourceSha: "sha", platform: "win32", results: [{ name: "cargo", code: 1, elapsedMs: 5000 }] }] }, { revision: "sha", dirty: true });
  assert.equal(result.local.records[0].freshness, "worktree-dirty");
  assert.equal(result.local.records[0].state, "failed");
  assert.equal(result.local.executed.run.p50Seconds, 5);
});

test("dependency installation is setup even when its name mentions tests", () => {
  const result = summarize({ runs: [run([job({ steps: [{ name: "Install test dependencies", status: "completed", conclusion: "success", started_at: at(360), completed_at: at(365) }] })])] });
  assert.equal(result.steps.setup.totalSeconds, 5);
  assert.equal(result.steps.testing.count, 0);
});

test("negative and nonnumeric local durations remain unmeasured", () => {
  const result = summarize({ local: [
    { name: "negative", code: 0, elapsedMs: -1 },
    { name: "string", code: 0, elapsedMs: "100" },
    { name: "bad queue", code: 0, elapsedMs: 100, queuedMs: 200 },
  ] });
  assert.equal(result.local.records[0].state, "unexecuted");
  assert.equal(result.local.records[1].state, "unexecuted");
  assert.equal(result.local.executed.run.count, 0);
});

test("collection caps run samples and paginates jobs for each latest attempt", async () => {
  const { collect } = await import("../validation/latency.mjs");
  const calls = [];
  const snapshot = collect("owner/repo", 1, endpoint => {
    calls.push(endpoint);
    if (endpoint.includes("/actions/runs?")) return { workflow_runs: [{ id: 10, run_attempt: 2 }, { id: 11, run_attempt: 1 }] };
    if (endpoint.endsWith("page=1")) return { total_count: 2, jobs: [{ id: 20 }] };
    return { total_count: 2, jobs: [{ id: 21 }] };
  });
  assert.equal(snapshot.runs.length, 1);
  assert.equal(snapshot.runs[0].jobs.length, 2);
  assert.equal(calls.length, 3);
  assert.match(calls[1], /runs\/10\/attempts\/2\/jobs/);
  assert.match(snapshot.capturedAt, /^\d{4}-/);
});

test("offline CLI output is replayable and defaults to attempts rather than success-only receipts", async () => {
  const { mkdtemp, mkdir, writeFile, readFile, rm } = await import("node:fs/promises");
  const { spawnSync } = await import("node:child_process");
  const { fileURLToPath } = await import("node:url");
  const path = await import("node:path");
  const root = fileURLToPath(new URL("../../", import.meta.url));
  await mkdir(path.join(root, "tmp"), { recursive: true });
  const directory = await mkdtemp(path.join(root, "tmp/latency-test-"));
  const cli = fileURLToPath(new URL("../validation/latency.mjs", import.meta.url));
  const execute = args => {
    const result = spawnSync(process.execPath, [cli, ...args], { cwd: directory, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout;
  };
  try {
    await mkdir(path.join(directory, "target/validation/attempts"), { recursive: true });
    await mkdir(path.join(directory, "target/validation/receipts"), { recursive: true });
    const record = { name: "cargo", source: "sha", code: 1, elapsedMs: 5000, executionMs: 4000, queuedMs: 1000 };
    await writeFile(path.join(directory, "target/validation/attempts/failed.json"), JSON.stringify(record));
    await writeFile(path.join(directory, "target/validation/receipts/passed.json"), JSON.stringify({ ...record, code: 0 }));
    const report = JSON.parse(execute(["--json"]));
    assert.match(report.source, /attempts/);
    assert.equal(report.local.records.length, 1);
    assert.equal(report.local.records[0].state, "failed");
    const filename = path.join(directory, "snapshot.json");
    await writeFile(filename, JSON.stringify(report));
    const output = path.join(directory, "output/report.json");
    assert.equal(execute(["--input", filename, "--json", "--output", output]), "");
    const replay = JSON.parse(await readFile(output, "utf8"));
    assert.deepEqual(replay.local, report.local);
    assert.deepEqual(replay.snapshot, report.snapshot);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

const feedback = extra => ({ schema: 1, kind: "development-feedback", source: "sha", dirty: true, platform: "linux", mode: "checks-and-tests",
  started: 1000, finished: 11000, status: "passed", phases: [{ name: "static", status: "passed", finished: 4000, commands: [{ started: 2000, finished: 3000, status: "passed" }] }], ...extra });

test("feedback metrics retain outcomes, platforms and reused evidence without invented defect rates", () => {
  const report = summarize({ feedback: [
    feedback(),
    feedback({ platform: "win32", status: "unavailable", finished: 7000, phases: [{ name: "static", status: "unavailable", finished: 6000, commands: [{ started: 2000, finished: 5000, status: "unavailable" }] }] }),
    feedback({ status: "running", finished: undefined, phases: [] }),
    feedback({ status: "passed", phases: [{ name: "tests", status: "passed", reused: true, commands: [] }] }),
  ] }, { revision: "sha", dirty: true });
  assert.equal(report.feedback.count, 4);
  assert.equal(report.feedback.firstCommand.p50Seconds, 2);
  assert.equal(report.feedback.firstIssue.p50Seconds, 4);
  assert.equal(report.feedback.passedCompletion.count, 2);
  assert.equal(report.feedback.outcomes.running, 1);
  assert.equal(report.feedback.outcomes.unavailable, 1);
  assert.equal(report.feedback.byPlatform.win32.completion.p95Seconds, 6);
  assert.equal(report.feedback.records[3].firstCommandSeconds, null);
  assert.equal(report.feedback.records[3].reusedPhases, 1);
  assert.match(formatReport(report), /first observed command/i);
});

test("CLI feedback collection excludes progress artifacts and replays snapshots without live appends", async t => {
  const { mkdtemp, mkdir, writeFile, readFile, rm } = await import("node:fs/promises");
  const { spawnSync } = await import("node:child_process");
  const { fileURLToPath } = await import("node:url");
  const path = await import("node:path");
  const root = await mkdtemp(path.join(fileURLToPath(new URL("../../tmp/", import.meta.url)), "feedback-latency-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const dir = path.join(root, "target/validation/feedback");
  await mkdir(dir, { recursive: true });
  await writeFile(path.join(dir, "run.json"), JSON.stringify(feedback({ timingOrigin: "invocation" })));
  await writeFile(path.join(dir, "progress.json"), JSON.stringify({ schema: 1, phase: "tests", commands: [] }));
  await writeFile(path.join(dir, "stage.json"), JSON.stringify({ schema: 1, name: "lint", code: 0, elapsedMs: 10 }));
  const cli = fileURLToPath(new URL("../validation/latency.mjs", import.meta.url));
  const execute = args => {
    const result = spawnSync(process.execPath, [cli, ...args, "--json"], { cwd: root, encoding: "utf8" });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    return JSON.parse(result.stdout);
  };
  const initial = execute([]);
  assert.equal(initial.feedback.count, 1);
  assert.equal(initial.feedback.records[0].firstCommandSeconds, 2);
  assert.equal(initial.feedback.records[0].timingOrigin, "invocation");
  const snapshot = path.join(root, "snapshot.json");
  await writeFile(snapshot, JSON.stringify(initial));
  await writeFile(path.join(dir, "later.json"), JSON.stringify(feedback({ platform: "win32", status: "cancelled", phases: [] })));
  const replay = execute(["--input", snapshot]);
  assert.deepEqual(replay.feedback, initial.feedback);
  assert.deepEqual(replay.snapshot, initial.snapshot);
  const appended = execute(["--input", snapshot, "--feedback-dir", dir]);
  assert.equal(appended.feedback.count, 2); // The existing run is refreshed, not counted twice.
  const output = path.join(root, "output.json");
  const result = spawnSync(process.execPath, [cli, "--input", snapshot, "--json", "--output", output], { cwd: root, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "");
  assert.equal(JSON.parse(await readFile(output, "utf8")).feedback.count, 1);
});

test("live observation refreshes one invocation while preserving later retries at the same path", () => {
  const report = summarize({ feedback: [
    feedback({ recordPath: "current.json", status: "running", finished: undefined, phases: [] }),
    feedback({ recordPath: "current.json", status: "failed" }),
    feedback({ recordPath: "current.json", started: 12000, finished: 22000, status: "passed", phases: [] }),
  ] });
  assert.equal(report.feedback.count, 2);
  assert.equal(report.feedback.outcomes.failed, 1);
  assert.equal(report.feedback.outcomes.passed, 1);
  assert.equal(report.feedback.outcomes.running, undefined);
});
