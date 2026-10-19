#!/usr/bin/env node
/** Offline serial libtest completion-gap estimates, never authoritative timings. */
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { stripVTControlCharacters } from "node:util";

function decode(line) {
  line = stripVTControlCharacters(line);
  const stamp = line.match(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})/u);
  return { timestamp: stamp && Number.isFinite(Date.parse(stamp[0])) ? Date.parse(stamp[0]) : null,
    stream: stamp ? line.slice(0, stamp.index).trim() : "",
    body: stamp ? line.slice(stamp.index + stamp[0].length).trim() : line.split("\t").at(-1).trim() };
}
export function profile(log, { serial = false, top = 20 } = {}) {
  if (!serial) throw Error("Explicit --serial is required; concurrent completion gaps cannot estimate test durations");
  if (!Number.isInteger(top) || top < 1 || top > 1000) throw Error("--top must be an integer from 1 to 1000");
  const tests = []; const suites = []; const diagnostics = [];
  let suite; let anchor = null; let context = "unknown binary"; let stream = "";
  const begin = (expected = null, timestamp = null) => {
    suite = { id: suites.length + 1, context, stream, expectedTests: expected, complete: false,
      declaredSeconds: null, estimatedSeconds: 0, measuredTests: 0, observedTests: 0, differenceSeconds: null };
    suites.push(suite); anchor = timestamp;
  };
  for (const [index, raw] of log.split(/\r?\n/u).entries()) {
    const { timestamp, body, stream: nextStream } = decode(raw);
    const running = body.match(/^running (\d+) tests?$/u);
    const row = body.match(/^test (.+?) \.\.\.(?:\s+(ok|FAILED|ignored)(?:,.*)?)?\s*$/u);
    const summary = body.startsWith("test result:");
    const binary = /^(?:Running\s+|Doc-tests\s+)/u.test(body);
    const build = /^(?:Compiling\s+|Finished\s+.+target\(s\))/u.test(body);
    if (!running && !row && !summary && !binary && !build) continue;
    if (nextStream !== stream && timestamp !== null) {
      if (suite && !suite.complete) diagnostics.push(`Line ${index + 1}: log stream changed; reset incomplete suite`);
      suite = undefined; anchor = null; context = "unknown binary"; stream = nextStream;
    }
    if (build) { context = "unknown binary"; suite = undefined; anchor = null; continue; }
    if (binary) { context = body; suite = undefined; anchor = null; continue; }
    if (running) { begin(Number(running[1]), timestamp); continue; }
    if (!suite || suite.complete) begin();
    if (summary) {
      const declared = body.match(/finished in (\d+(?:\.\d+)?)s\b/u);
      suite.declaredSeconds = declared ? Number(declared[1]) : null;
      suite.complete = suite.expectedTests === null || suite.expectedTests === suite.observedTests;
      if (!suite.complete) diagnostics.push(`Suite ${suite.id}: expected ${suite.expectedTests} rows, observed ${suite.observedTests}`);
      if (suite.declaredSeconds !== null) suite.differenceSeconds = suite.estimatedSeconds - suite.declaredSeconds;
      anchor = null; continue;
    }
    const state = { ok: "passed", FAILED: "failed", ignored: "ignored" }[row[2]] || "unfinished";
    let estimatedSeconds = null;
    if (state !== "ignored" && state !== "unfinished") {
      if (timestamp === null) { diagnostics.push(`Line ${index + 1}: missing or malformed timestamp`); anchor = null; }
      else if (anchor !== null && timestamp < anchor) { diagnostics.push(`Line ${index + 1}: nonmonotonic timestamp; reset timing anchor`); anchor = null; }
      else { estimatedSeconds = anchor === null ? null : (timestamp - anchor) / 1000; anchor = timestamp; }
    }
    if (state === "unfinished") anchor = null;
    suite.observedTests++;
    if (estimatedSeconds !== null) { suite.estimatedSeconds += estimatedSeconds; suite.measuredTests++; }
    tests.push({ test: row[1], state, estimatedSeconds, context: suite.context, stream: suite.stream, suite: suite.id, line: index + 1 });
  }
  for (const value of suites) {
    if (!value.complete) diagnostics.push(`Suite ${value.id}: unfinished or missing summary`);
    value.estimatedSeconds = Number(value.estimatedSeconds.toFixed(6));
    if (value.differenceSeconds !== null) value.differenceSeconds = Number(value.differenceSeconds.toFixed(6));
  }
  const measured = tests.filter(value => value.estimatedSeconds !== null);
  const families = new Map();
  for (const value of measured) {
    const family = value.test.includes("::") ? value.test.slice(0, value.test.lastIndexOf("::")) : "(root)";
    const key = JSON.stringify([value.context, value.stream, family]);
    const aggregate = families.get(key) || { family, context: value.context, stream: value.stream, estimatedSeconds: 0, measuredTests: 0, failedTests: 0 };
    aggregate.estimatedSeconds += value.estimatedSeconds; aggregate.measuredTests++; aggregate.failedTests += Number(value.state === "failed"); families.set(key, aggregate);
  }
  const descending = (a, b) => b.estimatedSeconds - a.estimatedSeconds;
  return { schema: 1, serialAssumption: true, metric: "completion-gap ESTIMATE", tests, suites, diagnostics,
    counts: Object.fromEntries(["passed", "failed", "ignored", "unfinished"].map(state => [state, tests.filter(value => value.state === state).length])),
    topTests: [...measured].sort(descending).slice(0, top),
    topFamilies: [...families.values()].map(value => ({ ...value, estimatedSeconds: Number(value.estimatedSeconds.toFixed(6)) })).sort(descending).slice(0, top),
    limitations: "Completion-gap ESTIMATE under an explicit serial execution assumption (for example RUST_TEST_THREADS=1). Gaps include harness/report overhead and timestamp buffering/chunk delivery; they are not authoritative per-test timings. JavaScript timestamps use millisecond precision. Ignored rows are not execution samples; failed and unfinished rows remain labeled separately. Suite and binary boundaries reset anchors and exclude build gaps. Missing/backwards timestamps yield unknown estimates. Family totals group the immediate parent test module within each binary/log stream. Compare estimated totals with declared suite durations as a diagnostic only; incomplete logs and finite samples establish no speedup." };
}
export function formatReport(report) {
  const lines = ["Serial libtest completion-gap ESTIMATE", `Observed states: ${JSON.stringify(report.counts)}`];
  for (const suite of report.suites) lines.push(`Suite ${suite.id}: ${suite.context}; complete=${suite.complete}; measured=${suite.measuredTests}/${suite.observedTests}; estimated sum=${suite.estimatedSeconds}s; declared=${suite.declaredSeconds ?? "unknown"}s; difference=${suite.differenceSeconds ?? "unknown"}s`);
  lines.push("Largest completion-gap estimates (passed/failed labels preserved):");
  for (const value of report.topTests) lines.push(`${value.estimatedSeconds.toFixed(3)}s [${value.state}] ${value.test} (${value.context}; line ${value.line})`);
  lines.push("Largest immediate module families:");
  for (const value of report.topFamilies) lines.push(`${value.estimatedSeconds.toFixed(3)}s ${value.family}; measured=${value.measuredTests}; failed=${value.failedTests}; ${value.context}`);
  lines.push(...report.diagnostics, report.limitations);
  return `${lines.join("\n")}\n`;
}
export function parseArgs(args) {
  const options = { top: 20 };
  for (let index = 0; index < args.length; index++) {
    const option = args[index];
    if (["--serial", "--json", "--help"].includes(option)) { options[option.slice(2)] = true; continue; }
    if (!["--input", "--top"].includes(option) || !args[index + 1] || args[index + 1].startsWith("--")) throw Error(`Invalid option or missing value: ${option}`);
    options[option.slice(2)] = args[++index];
  }
  if (options.help) return options;
  if (!options.input) throw Error("--input log file is required");
  if (!options.serial) throw Error("Explicit --serial is required; verify the source ran with RUST_TEST_THREADS=1");
  options.top = Number(options.top);
  if (!Number.isInteger(options.top) || options.top < 1 || options.top > 1000) throw Error("--top must be an integer from 1 to 1000");
  return options;
}
async function main(args) {
  const options = parseArgs(args);
  if (options.help) { console.log("Usage: node scripts/validation/libtest-timing.mjs --input timestamped-gh.log --serial [--top 20] [--json]\nOnly interpret known serial libtest pretty output, such as Windows jobs with RUST_TEST_THREADS=1. Offline completion-gap estimates include reporting overhead; no runner changes or speed claims."); return; }
  const report = { input: path.resolve(options.input), ...profile(await readFile(options.input, "utf8"), options) };
  process.stdout.write(options.json ? `${JSON.stringify(report, null, 2)}\n` : formatReport(report));
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 2; });
