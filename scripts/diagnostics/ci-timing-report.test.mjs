import assert from "node:assert/strict";
import test from "node:test";
import { percentile, summarizeRun, buildReport, readPages, collectReport } from "../ci/timing-report.mjs";

const stamp = seconds => new Date(Date.UTC(2026, 0, 1) + seconds * 1000).toISOString();
const run = (overrides = {}) => ({ id: 1, workflow_id: 10, name: "CI", event: "pull_request", head_sha: "a".repeat(40), run_attempt: 1, status: "completed", conclusion: "success", created_at: stamp(0), updated_at: stamp(120), ...overrides });
const job = (id, created, started, completed, extra = {}) => ({ id, name: `job-${id}`, status: "completed", conclusion: "success", created_at: stamp(created), started_at: stamp(started), completed_at: stamp(completed), ...extra });

test("nearest-rank percentiles require samples and do not manufacture zeros", () => {
  assert.equal(percentile([], 0.95), null);
  assert.equal(percentile([10, 1, 2, 3], 0.5), 2);
  assert.equal(percentile([10, 1, 2, 3], 0.95), 10);
  assert.throws(() => percentile([NaN], 0.5), /finite/);
});

test("dispatch intervals, summed execution, workflow span and latest-finishing tail remain distinct", () => {
  const result = summarizeRun({ ...run(), jobs: [job(1, 0, 20, 100), job(2, 0, 5, 60), job(3, 100, 110, 120)] });
  assert.equal(result.dispatchSeconds, 35);
  assert.equal(result.executionSeconds, 145);
  assert.equal(result.runnerMinutes, 145 / 60);
  assert.equal(result.observedJobSpanSeconds, 120);
  assert.equal(result.apiRunSpanSeconds, 120);
  assert.equal(result.tail.name, "job-3");
  assert.equal(result.tail.executionSeconds, 10);
});

test("skipped, queued, invalid timestamps and reruns are never zero-duration successes", () => {
  const result = summarizeRun({ ...run({ run_attempt: 2, status: "in_progress", conclusion: null }), jobs: [
    job(1, 0, 0, 0, { conclusion: "skipped" }),
    job(2, 20, 30, 40, { status: "queued", conclusion: null, started_at: null, completed_at: null }),
    job(3, 50, 40, 30),
  ] });
  assert.equal(result.complete, false);
  assert.equal(result.counts.skipped, 1);
  assert.equal(result.counts.incomplete, 1);
  assert.equal(result.runnerMinutes, 0);
  assert.equal(result.missingExecution, 1);
  assert.equal(result.observedJobSpanSeconds, null);
  assert.equal(result.tail, null);
});

test("duplicate run attempts are counted once and historical failures/skips/reruns remain visible", () => {
  const current = { ...run({ run_attempt: 2 }), jobs: [job(1, 100, 110, 120)] };
  const history = [run({ id: 2 }), run({ id: 3, conclusion: "failure", updated_at: stamp(30) }), run({ id: 4, conclusion: "skipped" }), run({ id: 5, run_attempt: 2 })];
  const report = buildReport({ repo: "o/r", sha: run().head_sha, runs: [run(), current], checks: [], statuses: [], histories: [{ workflowId: 10, event: "pull_request", runs: history }] });
  assert.equal(report.runs.length, 1);
  assert.equal(report.runs[0].attempt, 2);
  assert.equal(report.history[0].counts.failure, 1);
  assert.equal(report.history[0].counts.skipped, 1);
  assert.equal(report.history[0].excludedReruns, 1);
  assert.equal(report.history[0].durationSamples, 2);
  assert.equal(report.history[0].apiSpanP50Seconds, 30);
  assert.equal(report.history[0].apiSpanP95Seconds, 120);
});

test("pagination exhausts all pages, deduplicates IDs later and rejects API truncation", async () => {
  const calls = [];
  const rows = await readPages(async endpoint => { calls.push(endpoint); return { total_count: 101, jobs: calls.length === 1 ? Array.from({ length: 100 }, (_, id) => ({ id })) : [{ id: 100 }] }; }, "repos/o/r/jobs", "jobs");
  assert.equal(rows.length, 101);
  assert.match(calls[1], /page=2/);
  await assert.rejects(readPages(async () => ({ total_count: 101, jobs: [] }), "jobs", "jobs"), /incomplete/);
});

test("collector uses attempt endpoint and bounds historical records while keeping raw evidence", async () => {
  const requests = [];
  const current = run({ run_attempt: 2 });
  const report = await collectReport("o/r", run().head_sha, { history: 3, get: async endpoint => {
    requests.push(endpoint);
    if (endpoint.includes("/attempts/2/jobs")) return { total_count: 1, jobs: [job(1, 100, 110, 120)] };
    if (endpoint.includes("/workflows/10/runs")) return { workflow_runs: [run({ id: 2 })] };
    if (endpoint.includes("/actions/runs?")) return { total_count: 1, workflow_runs: [current, current] };
    if (endpoint.includes("/check-runs?")) return { total_count: 0, check_runs: [] };
    if (endpoint.includes("/statuses?")) return [];
    throw new Error(endpoint);
  } });
  assert.equal(requests.filter(value => value.includes("/attempts/")).length, 1);
  assert.ok(requests.some(value => value.includes("per_page=3") && value.includes("status=completed")));
  assert.equal(report.raw.runs[0].jobs.length, 1);
  assert.equal(report.runs[0].attempt, 2);
  assert.match(report.definitions.dispatch, /needs/);
});

test("real API skipped-only timestamp anomalies never produce negative workflow spans", () => {
  const skipped = summarizeRun({ ...run({ conclusion: "skipped" }), jobs: [job(1, 8, 0, 0, { conclusion: "skipped" })] });
  assert.equal(skipped.observedJobSpanSeconds, null);
  assert.equal(skipped.tail, null);
  const invalid = summarizeRun({ ...run(), jobs: [job(1, 80, 10, 20)] });
  assert.equal(invalid.observedJobSpanSeconds, null);
});
