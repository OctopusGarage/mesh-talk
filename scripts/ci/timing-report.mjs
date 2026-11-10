#!/usr/bin/env node
// Read-only GitHub Actions timing observations. No workflow dispatches or writes.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const timestamp = value => typeof value === "string" && Number.isFinite(Date.parse(value)) ? Date.parse(value) : null;
const seconds = (start, end) => {
  const a = timestamp(start), b = timestamp(end);
  return a !== null && b !== null && b >= a ? (b - a) / 1000 : null;
};
const unique = rows => [...new Map([...rows].sort((a, b) => (a.run_attempt ?? 0) - (b.run_attempt ?? 0) || (a.id ?? 0) - (b.id ?? 0)).map(row => [row.id, row])).values()];
const sum = rows => rows.reduce((total, value) => total + (value ?? 0), 0);

export function percentile(values, fraction) {
  assert.ok(fraction > 0 && fraction <= 1, "percentile fraction must be in (0, 1]");
  assert.ok(values.every(Number.isFinite), "percentile samples must be finite");
  return values.length ? [...values].sort((a, b) => a - b)[Math.ceil(values.length * fraction) - 1] : null;
}

export function summarizeRun(run) {
  const jobs = unique(run.jobs ?? []).map(job => ({
    id: job.id, name: job.name, status: job.status, conclusion: job.conclusion,
    createdAt: job.created_at, startedAt: job.started_at, completedAt: job.completed_at,
    dispatchSeconds: job.conclusion === "skipped" ? null : seconds(job.created_at, job.started_at),
    executionSeconds: job.status === "completed" && job.conclusion !== "skipped" ? seconds(job.started_at, job.completed_at) : null,
  }));
  const counts = { success: 0, failure: 0, skipped: 0, incomplete: 0, other: 0 };
  for (const job of jobs) {
    if (job.status !== "completed") counts.incomplete++;
    else if (job.conclusion === "success") counts.success++;
    else if (job.conclusion === "skipped") counts.skipped++;
    else if (["failure", "timed_out", "cancelled", "action_required", "startup_failure"].includes(job.conclusion)) counts.failure++;
    else counts.other++;
  }
  const missingExecution = jobs.filter(job => job.status === "completed" && job.conclusion !== "skipped" && job.executionSeconds === null).length;
  const complete = run.status === "completed" && counts.incomplete === 0 && missingExecution === 0 && jobs.length > 0;
  const executed = jobs.filter(job => job.executionSeconds !== null);
  const tail = complete ? [...executed].sort((a, b) => timestamp(b.completedAt) - timestamp(a.completedAt))[0] ?? null : null;
  const starts = jobs.map(job => timestamp(job.createdAt) ?? timestamp(job.startedAt)).filter(value => value !== null);
  const ends = jobs.map(job => timestamp(job.completedAt)).filter(value => value !== null);
  const observedSpan = complete && executed.length && starts.length && ends.length ? (Math.max(...ends) - Math.min(...starts)) / 1000 : null;
  const span = observedSpan !== null && observedSpan >= 0 ? observedSpan : null;
  return {
    id: run.id, workflowId: run.workflow_id, name: run.name, event: run.event,
    attempt: run.run_attempt, status: run.status, conclusion: run.conclusion, url: run.html_url,
    complete, counts, missingExecution,
    missingDispatch: jobs.filter(job => job.conclusion !== "skipped" && job.dispatchSeconds === null).length,
    dispatchSeconds: sum(jobs.map(job => job.dispatchSeconds)),
    executionSeconds: sum(jobs.map(job => job.executionSeconds)),
    runnerMinutes: sum(jobs.map(job => job.executionSeconds)) / 60,
    observedJobSpanSeconds: span,
    apiRunSpanSeconds: run.status === "completed" ? seconds(run.created_at, run.updated_at) : null,
    tail, jobs,
  };
}

function historySummary(history, currentIds) {
  const runs = unique(history.runs).filter(run => !currentIds.has(run.id));
  const eligible = runs.filter(run => run.status === "completed" && run.run_attempt === 1 && !["skipped", "neutral"].includes(run.conclusion));
  const durations = eligible.map(run => seconds(run.created_at, run.updated_at)).filter(value => value !== null);
  const counts = {};
  for (const run of runs) counts[run.conclusion ?? run.status] = (counts[run.conclusion ?? run.status] ?? 0) + 1;
  return {
    workflowId: history.workflowId, event: history.event, sampledRuns: runs.length, counts,
    excludedReruns: runs.filter(run => run.run_attempt > 1).length,
    durationSamples: durations.length, apiSpanP50Seconds: percentile(durations, 0.5), apiSpanP95Seconds: percentile(durations, 0.95),
  };
}

export function buildReport(snapshot) {
  const raw = { ...snapshot, runs: unique(snapshot.runs) };
  const runs = raw.runs.map(summarizeRun);
  const latestChecks = new Map();
  for (const check of [...raw.checks].sort((a, b) => a.id - b.id)) latestChecks.set(`${check.app?.id}:${check.name}`, check);
  const latestStatuses = new Map();
  for (const status of [...raw.statuses].sort((a, b) => a.id - b.id)) latestStatuses.set(status.context, status);
  const completedJobs = runs.flatMap(run => run.jobs).filter(job => job.executionSeconds !== null);
  return {
    schemaVersion: 1, capturedAt: snapshot.capturedAt ?? new Date().toISOString(), repo: snapshot.repo, sha: snapshot.sha,
    definitions: {
      dispatch: "Job created_at → started_at. Includes upstream needs/dependency and dispatch waits; NOT exact runnable queue time.",
      execution: "Job started_at → completed_at for non-skipped completed jobs; missing/in-progress durations stay unknown. Runner minutes sum known execution, not billing or wall time.",
      workflowSpan: "observedJobSpanSeconds spans first current-attempt job creation to last completion only when all observed jobs complete. apiRunSpanSeconds is created_at → updated_at metadata span, including previous attempts and finalization updates.",
      tail: "Latest-finishing executed job in a complete observed workflow, not a reconstructed DAG critical path.",
      history: "Bounded most-recent completed records per workflow and event; current run IDs excluded. Nearest-rank P50/P95 of API created→updated spans for first-attempt, non-skipped/non-neutral runs, including failures. Configuration/runner changes are not controlled; historical job execution/queue is not inferred.",
      snapshot: "API reads are not atomic; attempts/status can change during collection. No acceptance verdict is inferred from timing.",
    },
    complete: runs.length > 0 && runs.every(run => run.complete),
    knownRunnerMinutes: sum(completedJobs.map(job => job.executionSeconds)) / 60,
    runs,
    history: (raw.histories ?? []).map(history => historySummary(history, new Set(raw.runs.map(run => run.id)))),
    finalChecks: [...latestChecks.values()].map(check => ({ id: check.id, name: check.name, app: check.app?.slug, status: check.status, conclusion: check.conclusion })),
    finalStatuses: [...latestStatuses.values()].map(status => ({ id: status.id, context: status.context, state: status.state })),
    raw,
  };
}

export async function readPages(get, endpoint, key) {
  const rows = [];
  let expected;
  for (let page = 1; ; page++) {
    const result = await get(`${endpoint}${endpoint.includes("?") ? "&" : "?"}per_page=100&page=${page}`);
    const values = key ? result[key] : result;
    assert.ok(Array.isArray(values), `Invalid GitHub page for ${endpoint}`);
    if (Number.isInteger(result.total_count)) expected = result.total_count;
    rows.push(...values);
    if (values.length < 100) break;
  }
  assert.ok(expected === undefined || new Set(rows.map(row => row.id)).size >= expected, `GitHub pagination incomplete or search capped: ${endpoint}`);
  return rows;
}

export async function collectReport(repo, sha, { get, history = 0 } = {}) {
  assert.match(repo, /^[\w.-]+\/[\w.-]+$/u);
  assert.match(sha, /^[a-f0-9]{40}$/u);
  assert.ok(Number.isInteger(history) && history >= 0 && history <= 100, "history must be 0..100");
  get ??= endpoint => JSON.parse(execFileSync("gh", ["api", "--method", "GET", endpoint], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 }));
  const base = `repos/${repo}`;
  const runs = unique(await readPages(get, `${base}/actions/runs?head_sha=${sha}`, "workflow_runs"));
  for (const run of runs) {
    assert.equal(run.head_sha, sha, "GitHub returned another source revision");
    assert.ok(Number.isInteger(run.run_attempt) && run.run_attempt > 0, "Missing run attempt");
    run.jobs = unique(await readPages(get, `${base}/actions/runs/${run.id}/attempts/${run.run_attempt}/jobs`, "jobs"));
  }
  const checks = await readPages(get, `${base}/commits/${sha}/check-runs?filter=all`, "check_runs");
  const statuses = await readPages(get, `${base}/commits/${sha}/statuses`);
  const histories = [];
  const groups = new Map(runs.map(run => [`${run.workflow_id}:${run.event}`, run]));
  if (history) for (const run of groups.values()) {
    const result = await get(`${base}/actions/workflows/${run.workflow_id}/runs?event=${encodeURIComponent(run.event)}&status=completed&per_page=${history}`);
    assert.ok(Array.isArray(result.workflow_runs), "Invalid history page");
    histories.push({ workflowId: run.workflow_id, event: run.event, runs: result.workflow_runs.slice(0, history) });
  }
  return buildReport({ repo, sha, capturedAt: new Date().toISOString(), runs, checks, statuses, histories });
}

export function formatReport(report) {
  const number = value => value === null ? "unknown" : value.toFixed(1);
  const lines = [`${report.repo} @ ${report.sha}`, `Observed ${report.runs.length} workflows; known runner execution ${number(report.knownRunnerMinutes)} min (not billing); snapshot ${report.complete ? "complete" : "incomplete"}.`];
  for (const run of report.runs) lines.push(`${run.name} [${run.event}, attempt ${run.attempt}] ${run.status}/${run.conclusion ?? "pending"}: known dispatch ${number(run.dispatchSeconds)}s (${run.missingDispatch} unknown), execution ${number(run.runnerMinutes)} min, job span ${number(run.observedJobSpanSeconds)}s; tail ${run.tail?.name ?? "unknown"}; failed ${run.counts.failure}, skipped ${run.counts.skipped}, incomplete ${run.counts.incomplete}.`);
  for (const sample of report.history) lines.push(`History workflow ${sample.workflowId} [${sample.event}]: n=${sample.durationSamples}, API span P50=${number(sample.apiSpanP50Seconds)}s P95=${number(sample.apiSpanP95Seconds)}s; conclusions ${JSON.stringify(sample.counts)}; excluded reruns=${sample.excludedReruns}.`);
  lines.push(`Checks: ${report.finalChecks.map(check => `${check.name}=${check.conclusion ?? check.status}`).join(", ") || "none observed"}`);
  lines.push(`Statuses: ${report.finalStatuses.map(status => `${status.context}=${status.state}`).join(", ") || "none observed"}`);
  lines.push(...Object.values(report.definitions));
  return `${lines.join("\n")}\n`;
}

async function main(args) {
  const [repo, sha, ...flags] = args;
  let history = 0, json = false;
  while (flags.length) {
    const flag = flags.shift();
    if (flag === "--json") json = true;
    else if (flag === "--history" && flags.length) history = Number(flags.shift());
    else throw new Error("Usage: timing-report.mjs OWNER/REPO SHA [--history 0..100] [--json]");
  }
  const report = await collectReport(repo, sha, { history });
  process.stdout.write(json ? `${JSON.stringify(report, null, 2)}\n` : formatReport(report));
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1; });
