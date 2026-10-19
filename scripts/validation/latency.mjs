#!/usr/bin/env node
/** Read-only measurements. Nearest-rank quantiles; missing intervals stay unknown. */
import { spawnSync } from "node:child_process";
import { readFile, readdir, mkdir, writeFile, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { feedbackTiming } from "./feedback.mjs";

const states = ["completed", "cancelled", "unfinished", "skipped"];
const timestamp = value => typeof value === "string" && Number.isFinite(Date.parse(value)) ? Date.parse(value) : null;
const seconds = value => typeof value === "number" && Number.isFinite(value) && value >= 0 ? value / 1000 : null;
function interval(start, end) {
  const a = timestamp(start); const b = timestamp(end);
  return a !== null && b !== null && b >= a ? (b - a) / 1000 : null;
}
export function quantile(values, fraction) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.max(0, Math.ceil(fraction * sorted.length) - 1)];
}
function distribution(values) {
  const measured = values.filter(value => value !== null);
  return { count: measured.length, p50Seconds: quantile(measured, 0.5), p95Seconds: quantile(measured, 0.95), totalSeconds: measured.reduce((sum, value) => sum + value, 0) };
}
function state(job) {
  if (job.status !== "completed") return "unfinished";
  if (job.conclusion === "cancelled") return "cancelled";
  if (job.conclusion === "skipped") return "skipped";
  return "completed";
}
function os(job) {
  const labels = `${(job.labels || []).join(" ")} ${job.runner_name || ""} ${job.name || ""}`.toLowerCase();
  if (/windows|win32/.test(labels)) return "windows";
  if (/macos|darwin|mac-/.test(labels)) return "macos";
  if (/ubuntu|linux/.test(labels)) return "linux";
  return "unknown";
}
function category(name) {
  const value = name.toLowerCase();
  if (/setup|set up|install|checkout|cache|download|prepare|restore/.test(value)) return "setup";
  if (/compile|compil|build|clippy|typecheck|type.check/.test(value)) return "compilation";
  if (/test|e2e|playwright|coverage|vitest|mutation/.test(value)) return "testing";
  return "other";
}
function groupJobs(jobs) {
  return Object.fromEntries(states.map(name => {
    const records = jobs.filter(job => job.state === name);
    const run = distribution(records.map(job => job.runSeconds));
    return [name, { count: records.length, queue: distribution(records.map(job => job.queueSeconds)), run, runnerMinutes: run.totalSeconds / 60 }];
  }));
}
function localRecords(records, current) {
  return records.flatMap(record => {
    // Portable guest reports contain individual step durations and a report-wide source SHA.
    if (Array.isArray(record.results)) return localRecords(record.results.map(result => ({ ...result, source: record.sourceSha, platform: record.platform, recordPath: record.recordPath, finished: record.finished, code: result.code })), current);
    const sourceSha = record.sourceSha || record.source || null;
    const execution = seconds(record.executionMs ?? (typeof record.elapsedMs === "number" && typeof record.queuedMs === "number" ? record.elapsedMs - record.queuedMs : record.elapsedMs));
    const validDurations = ["executionMs", "elapsedMs", "queuedMs"].every(key => record[key] === undefined || seconds(record[key]) !== null);
    const measured = Number.isInteger(record.code) && validDurations && execution !== null &&
      !(record.queuedMs !== undefined && record.elapsedMs !== undefined && record.queuedMs > record.elapsedMs);
    return [{ name: record.name || "unnamed", sourceSha, currentRevision: current.revision ?? null,
      freshness: !sourceSha || !current.revision ? "unknown" : sourceSha !== current.revision ? "stale" : current.dirty ? "worktree-dirty" : "same-revision",
      state: record.reused ? "reused" : !measured ? "unexecuted" : record.code === 130 ? "cancelled" : record.code === 0 ? "completed" : "failed",
      queueSeconds: measured && !record.reused ? seconds(record.queuedMs) : null,
      runSeconds: measured && !record.reused ? execution : null,
      savedExecutionSeconds: record.reused ? seconds(record.savedExecutionMs) : null,
      platform: record.platform || "unknown", finished: record.finished ?? null, recordPath: record.recordPath ?? null }];
  });
}
function feedbackSummary(reports, current) {
  // An explicit live append can revisit a run already present in the snapshot.
  // Keep its latest observation, without erasing another invocation at that path.
  const observed = new Map();
  for (const [index, report] of reports.entries()) {
    if (report?.schema !== 1 || report.kind !== "development-feedback" || !Array.isArray(report.phases)) continue;
    const identifiable = typeof report.recordPath === "string" && typeof report.started === "number" && Number.isFinite(report.started);
    observed.set(identifiable ? JSON.stringify([report.recordPath, report.started]) : `anonymous:${index}`, report);
  }
  const records = [...observed.values()].map(report => ({
    sourceSha: report.source ?? null, dirty: report.dirty ?? null, platform: report.platform || "unknown", mode: report.mode ?? "unknown",
    timingOrigin: report.timingOrigin ?? "legacy-report-start", status: report.status || "unknown", recordPath: report.recordPath ?? null,
    currentRevision: current.revision ?? null,
    freshness: !report.source || !current.revision ? "unknown" : report.source !== current.revision ? "stale" : current.dirty ? "worktree-dirty" : "same-revision",
    ...feedbackTiming(report),
  }));
  const group = items => ({ count: items.length,
    outcomes: Object.fromEntries([...new Set(items.map(item => item.status))].sort().map(status => [status, items.filter(item => item.status === status).length])),
    firstCommand: distribution(items.map(item => item.firstCommandSeconds)),
    firstIssue: distribution(items.map(item => item.firstIssue?.seconds ?? null)),
    completion: distribution(items.map(item => item.completionSeconds)),
    passedCompletion: distribution(items.filter(item => item.status === "passed").map(item => item.completionSeconds)),
  });
  return { ...group(records), records,
    byPlatform: Object.fromEntries([...new Set(records.map(item => item.platform))].sort().map(platform => [platform, group(records.filter(item => item.platform === platform))])),
    method: "Times are wall-clock observations from each report's declared start, not test proof or compute cost. Same-file/same-start observations use the last supplied record; separate invocations remain distinct. Legacy reports may start after planning. First command can be compilation, inventory or a cached stage wrapper; reused phase evidence invents no command result. Issues retain failed/unavailable/invalidated/blocked kinds; cancellation alone is not a failed assertion. Missing or invalid timestamps stay unknown. Completion includes all outcomes; passedCompletion means only the selected development scope. No regression, flake or escape rate is inferred.",
  };
}

export function summarize(snapshot = {}, current = {}) {
  if (!Array.isArray(snapshot.runs ?? []) || !Array.isArray(snapshot.local ?? [])) throw Error("Snapshot runs and local fields must be arrays");
  if (!Array.isArray(snapshot.feedback ?? [])) throw Error("Snapshot feedback must be an array");
  const normalized = [];
  const steps = { setup: [], compilation: [], testing: [], other: [] };
  const runs = (snapshot.runs || []).map(run => {
    if (!Array.isArray(run.jobs ?? [])) throw Error("Run jobs must be an array");
    const jobs = (run.jobs || []).map(job => {
      const status = state(job);
      const timed = status === "completed" || status === "cancelled";
      // Queued GitHub jobs can have a synthetic started_at. Only finished jobs
      // with a real runner interval participate in duration distributions.
      const runSeconds = timed ? interval(job.started_at, job.completed_at) : null;
      const item = { id: job.id, runId: run.id, name: job.name, state: status, os: os(job),
        queueSeconds: runSeconds === null ? null : interval(job.created_at, job.started_at), runSeconds };
      normalized.push(item);
      if (status === "completed" && runSeconds !== null) {
        for (const step of job.steps || []) {
          if (step.status !== "completed" || ["skipped", "cancelled"].includes(step.conclusion)) continue;
          const duration = interval(step.started_at, step.completed_at);
          if (duration !== null && timestamp(step.started_at) >= timestamp(job.started_at) && timestamp(step.completed_at) <= timestamp(job.completed_at)) steps[category(step.name || "")].push(duration);
        }
      }
      return { ...item, started: timestamp(job.started_at), finished: timestamp(job.completed_at) };
    });
    const allMeasured = run.status === "completed" && jobs.length > 0 && jobs.every(job => job.state === "skipped" || ((job.state === "completed" || job.state === "cancelled") && job.runSeconds !== null));
    const executed = jobs.filter(job => job.runSeconds !== null);
    const first = executed.length ? Math.min(...executed.map(job => job.started)) : null;
    const last = executed.length ? Math.max(...executed.map(job => job.finished)) : null;
    const created = timestamp(run.created_at);
    const elapsedSeconds = allMeasured && created !== null && last !== null && last >= created ? (last - created) / 1000 : null;
    return { id: run.id ?? null, sourceSha: run.head_sha ?? null, state: state(run), jobCount: jobs.length,
      elapsedSeconds, executionSpanSeconds: allMeasured && first !== null ? (last - first) / 1000 : null };
  });
  const local = localRecords(snapshot.local || [], current);
  const executed = local.filter(record => ["completed", "failed"].includes(record.state));
  return { schema: 1, source: snapshot.source || "offline snapshot", capturedAt: snapshot.capturedAt || null,
    sample: snapshot.sample || { runs: runs.length }, currentRevision: current.revision ?? null, worktreeDirty: current.dirty ?? null,
    method: "Nearest-rank quantiles: sorted value at ceil(p * n), no interpolation. Seconds; null means unknown. Queue includes dependency waits. Runner-minutes sum measured job execution (not billing). Run elapsed is created_at to last job completion; execution span is first job start to last completion, including gaps. These envelopes are observed critical-path proxies, not sums or a dependency-DAG analysis. Only fully observed job sets get envelopes. Steps are heuristic name categories for completed jobs only. Local same-revision is provenance, not proof that evidence is reusable; receipts are historical executions and absent records mean unmeasured, not passed.",
    feedback: feedbackSummary(snapshot.feedback ?? [], current),
    jobs: groupJobs(normalized), byOs: Object.fromEntries([...new Set(normalized.map(job => job.os))].sort().map(name => [name, groupJobs(normalized.filter(job => job.os === name))])),
    byJob: Object.fromEntries([...new Set(normalized.map(job => job.name || "unnamed"))].sort().map(name => [name, groupJobs(normalized.filter(job => (job.name || "unnamed") === name))])),
    steps: Object.fromEntries(Object.entries(steps).map(([name, values]) => [name, distribution(values)])), runs,
    local: { records: local, executed: { count: executed.length, run: distribution(executed.map(record => record.runSeconds)), queue: distribution(executed.map(record => record.queueSeconds)) } } };
}
const display = value => value === null ? "unknown" : `${Number(value.toFixed(2))}`;
export function formatReport(report) {
  const lines = ["Validation latency", `Source: ${report.source}; captured: ${report.capturedAt || "unknown"}`, `Current revision: ${report.currentRevision || "unknown"}; dirty: ${report.worktreeDirty ?? "unknown"}`, `Sample: ${JSON.stringify(report.sample)}`, "Job durations (seconds; p50/p95; measured samples):"];
  const row = (name, value) => `${name}: jobs=${value.count}; queue=${display(value.queue.p50Seconds)}/${display(value.queue.p95Seconds)} (n=${value.queue.count}); run=${display(value.run.p50Seconds)}/${display(value.run.p95Seconds)} (n=${value.run.count}); runner-minutes=${display(value.runnerMinutes)}`;
  for (const name of states) lines.push(row(name, report.jobs[name]));
  for (const [name, groups] of Object.entries(report.byOs)) for (const state of states) if (groups[state].count) lines.push(row(`${name}/${state}`, groups[state]));
  for (const [name, groups] of Object.entries(report.byJob)) for (const state of states) if (groups[state].count) lines.push(row(`Job ${name}/${state}`, groups[state]));
  for (const [name, value] of Object.entries(report.steps)) lines.push(`Steps/${name}: total=${display(value.totalSeconds)}s; p50/p95=${display(value.p50Seconds)}/${display(value.p95Seconds)}; n=${value.count}`);
  for (const run of report.runs) lines.push(`Run ${run.id}: ${run.state}; elapsed=${display(run.elapsedSeconds)}s; execution-span=${display(run.executionSpanSeconds)}s; SHA=${run.sourceSha || "unknown"}`);
  lines.push(`Local historical executions: ${report.local.executed.count}; run p50/p95=${display(report.local.executed.run.p50Seconds)}/${display(report.local.executed.run.p95Seconds)}s; queue p50/p95=${display(report.local.executed.queue.p50Seconds)}/${display(report.local.executed.queue.p95Seconds)}s`);
  for (const record of report.local.records) lines.push(`Local ${record.name}: ${record.state}; ${record.freshness}; SHA=${record.sourceSha || "unknown"}; run=${display(record.runSeconds)}s`);
  if (report.feedback?.count) {
    const timing = report.feedback;
    lines.push(`Development feedback: ${timing.count}; first observed command p50/p95=${display(timing.firstCommand.p50Seconds)}/${display(timing.firstCommand.p95Seconds)}s; first actionable issue=${display(timing.firstIssue.p50Seconds)}/${display(timing.firstIssue.p95Seconds)}s; selected-scope passed completion=${display(timing.passedCompletion.p50Seconds)}/${display(timing.passedCompletion.p95Seconds)}s`);
    lines.push(`Development outcomes: ${JSON.stringify(timing.outcomes)}`);
    for (const record of timing.records) lines.push(`Feedback ${record.platform}/${record.mode}: ${record.status}; ${record.freshness}; first issue=${record.firstIssue?.kind ?? "unobserved"}; reused phases=${record.reusedPhases}; origin=${record.timingOrigin}`);
    lines.push(timing.method);
  }
  if (report.feedback && !report.feedback.count) lines.push("Development feedback: no observations; first command/issue/completion timing unknown.");
  lines.push(report.method);
  return `${lines.join("\n")}\n`;
}
export function parseArgs(args) {
  const options = { limit: 20, json: false };
  for (let index = 0; index < args.length; index++) {
    const option = args[index];
    if (option === "--json") { options.json = true; continue; }
    if (option === "--help") { options.help = true; continue; }
    const names = { "--input": "input", "--repo": "repo", "--limit": "limit", "--output": "output", "--local-dir": "localDir", "--feedback-dir": "feedbackDir" };
    if (!names[option] || !args[index + 1] || args[index + 1].startsWith("--")) throw Error(`Unknown option or missing value: ${option}`);
    options[names[option]] = args[++index];
  }
  options.limit = Number(options.limit);
  if (!Number.isInteger(options.limit) || options.limit < 1 || options.limit > 100) throw Error("--limit must be an integer from 1 to 100");
  if (options.repo && !/^[\w.-]+\/[\w.-]+$/.test(options.repo)) throw Error("--repo must be owner/repo");
  if (options.repo && options.input) throw Error("Choose --input or --repo");
  return options;
}
function command(program, args) {
  const result = spawnSync(program, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, timeout: 120_000, shell: false });
  if (result.status !== 0) throw Error(result.stderr?.trim() || result.error?.message || `${program} failed`);
  return result.stdout.trim();
}
export function collect(repo, limit, api = endpoint => JSON.parse(command("gh", ["api", endpoint]))) {
  const response = api(`repos/${repo}/actions/runs?per_page=${limit}`);
  const runs = response.workflow_runs.slice(0, limit);
  for (const run of runs) {
    run.jobs = [];
    let page = 1;
    while (true) {
      const response = api(`repos/${repo}/actions/runs/${run.id}/attempts/${run.run_attempt}/jobs?per_page=100&page=${page}`);
      run.jobs.push(...response.jobs);
      if (run.jobs.length >= response.total_count) break;
      if (!response.jobs.length || page >= 100) throw Error(`Incomplete job pagination for run ${run.id}`);
      page++;
    }
  }
  return { source: `GitHub Actions ${repo}`, capturedAt: new Date().toISOString(), sample: { limit, runs: runs.length, selection: "Most recent workflow runs, all statuses, latest attempt per run; jobs paginated. Mutable API observed sequentially, not an atomic snapshot." }, runs };
}
async function readLocal(directory, feedback = false) {
  let entries;
  try { entries = await readdir(directory, { withFileTypes: true }); }
  catch (error) { if (error.code === "ENOENT") return []; throw error; }
  const records = [];
  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    const filename = path.join(directory, entry.name);
    if (entry.isDirectory()) records.push(...await readLocal(filename, feedback));
    else if (entry.isFile() && entry.name.endsWith(".json")) {
      const record = JSON.parse(await readFile(filename, "utf8"));
      if (feedback ? record.schema === 1 && record.kind === "development-feedback" && Array.isArray(record.phases) : record.name || Array.isArray(record.results)) records.push({ ...record, recordPath: filename });
    }
  }
  return records;
}
async function defaultLocalDirectory() {
  try { if ((await stat("target/validation/attempts")).isDirectory()) return "target/validation/attempts"; }
  catch (error) { if (error.code !== "ENOENT") throw error; }
  return "target/validation/receipts";
}
async function main(args) {
  const options = parseArgs(args);
  if (options.help) {
    console.log("Usage: node scripts/validation/latency.mjs [--input snapshot.json | --repo owner/repo] [--limit 1..100] [--local-dir directory] [--feedback-dir directory] [--json] [--output path]\nOffline snapshot: {source, capturedAt, runs: [{...REST workflow run, jobs: [...REST jobs]}], local: [...stage receipts or portable guest reports], feedback: [...development feedback reports]}. Without input/repo, reads target/validation/attempts (falling back to success-only legacy receipts) and target/validation/feedback. --local-dir and --feedback-dir explicitly add their respective local records to an input or GitHub sample. Read-only gh collection requires existing authentication. No automatic file writes; --output explicitly writes a report, preferably under ignored tmp/. JSON reports include the raw snapshot for offline replay.");
    return;
  }
  let current = {};
  try { current = { revision: command("git", ["rev-parse", "HEAD"]), dirty: command("git", ["status", "--porcelain", "--untracked-files=normal"]) !== "" }; } catch { /* Non-Git snapshots remain useful. */ }
  const localDirectory = options.localDir || await defaultLocalDirectory();
  let snapshot = options.input ? JSON.parse(await readFile(options.input, "utf8")) : options.repo ? collect(options.repo, options.limit) : { source: `local ${localDirectory}${localDirectory.endsWith("/receipts") ? " (success-only legacy receipts; failures/cancellations unavailable)" : ""}`, capturedAt: new Date().toISOString() };
  // --json output can itself be replayed via --input without losing raw samples.
  snapshot = snapshot.snapshot || snapshot;
  if (options.input && !snapshot.source) snapshot.source = `file:${path.resolve(options.input)}`;
  if (options.localDir || (!options.input && !options.repo)) snapshot.local = [...snapshot.local || [], ...await readLocal(path.resolve(localDirectory))];
  if (options.feedbackDir || (!options.input && !options.repo)) snapshot.feedback = [...snapshot.feedback || [], ...await readLocal(path.resolve(options.feedbackDir || "target/validation/feedback"), true)];
  const report = summarize(snapshot, current);
  const output = options.json ? `${JSON.stringify({ ...report, snapshot }, null, 2)}\n` : formatReport(report);
  if (options.output) { await mkdir(path.dirname(path.resolve(options.output)), { recursive: true }); await writeFile(options.output, output); }
  else process.stdout.write(output);
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1; });
