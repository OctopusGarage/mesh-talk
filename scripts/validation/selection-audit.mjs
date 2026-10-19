#!/usr/bin/env node
/** Shadow comparison only. Full validation remains required. No result collection. */
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const kinds = ["rust", "frontend", "node", "browser", "native"];
const sha = value => typeof value === "string" && /^[a-f0-9]{40}$/.test(value);
const nonempty = value => typeof value === "string" && value.trim().length > 0;
function file(value) {
  if (!nonempty(value) || /[\u0000-\u001f:]/u.test(value)) throw Error("Invalid test path");
  const normalized = value.replaceAll("\\", "/");
  if (normalized.startsWith("/") || normalized.split("/").includes("..")) throw Error("Invalid test path: absolute paths and traversal are forbidden");
  const result = normalized.split("/").filter(part => part && part !== ".").join("/");
  if (!result) throw Error("Invalid empty test path");
  return result;
}
function frontendFile(value) {
  const normalized = file(value);
  return normalized.startsWith("frontend/") ? normalized : `frontend/${normalized}`;
}
function validatePlan(plan) {
  if (!plan || typeof plan !== "object" || Array.isArray(plan)) throw Error("Invalid affected plan");
  if (plan.blockers !== undefined && (!Array.isArray(plan.blockers) || plan.blockers.some(value => typeof value !== "string"))) throw Error("Invalid plan blockers: expected an array of strings");
  for (const field of ["rust", "frontend", "node"]) if (!Array.isArray(plan[field])) throw Error(`Invalid plan ${field}: expected an array`);
  for (const target of plan.rust) {
    if (!target || !nonempty(target.package) || !Array.isArray(target.filters) || target.filters.some(value => !nonempty(value))) throw Error("Invalid Rust package/filter selection");
  }
  return { rust: plan.rust, frontend: new Set(plan.frontend.map(frontendFile)), node: new Set(plan.node.map(file)) };
}
function validateFailure(failure) {
  if (!failure || typeof failure !== "object" || Array.isArray(failure) || !kinds.includes(failure.kind)) throw Error("Invalid failure kind");
  const item = { ...failure };
  if (failure.kind === "rust" && (!nonempty(failure.package) || !nonempty(failure.test))) throw Error("Invalid Rust failure: package and test are required");
  if (failure.kind === "rust" && failure.target !== undefined && !nonempty(failure.target)) throw Error("Invalid Rust failure target");
  if (["frontend", "node"].includes(failure.kind) && !nonempty(failure.path)) throw Error("Invalid failure: a test file path is required");
  if (["browser", "native"].includes(failure.kind) && !nonempty(failure.path) && !nonempty(failure.test)) throw Error("Invalid escalation failure: path or test is required");
  if (failure.path !== undefined) item.path = failure.kind === "frontend" ? frontendFile(failure.path) : file(failure.path);
  return item;
}
export function summarize(plan, results) {
  const selection = validatePlan(plan);
  if (!results || typeof results !== "object" || !Array.isArray(results.failures)) throw Error("Invalid broader results: failures must be an explicit array");
  if (results.complete !== undefined && typeof results.complete !== "boolean") throw Error("Invalid broader results: complete must be boolean");
  const failures = results.failures.map(validateFailure);
  const reasons = [];
  if (plan.blockers?.length) reasons.push("Affected plan is blocked");
  if (!sha(plan.source)) reasons.push("Plan has no valid source SHA");
  if (plan.dirty !== false) reasons.push("Plan must explicitly identify a clean worktree");
  if (!sha(results.source)) reasons.push("Broader results have no valid source SHA");
  if (plan.source !== results.source) reasons.push("Plan and broader results belong to different source revisions");
  if (results.complete !== true) reasons.push("Broader results are incomplete or completeness is unspecified");
  const report = { schema: 1, planSource: plan.source ?? null, resultsSource: results.source ?? null,
    qualified: reasons.length === 0, status: "unqualified", reasons, observedFailureCount: failures.length,
    selectedFailures: null, misses: null, escalations: null, firstMissedRegression: null, exitCode: 2,
    suites: results.suites ?? null,
    limitations: "This is one finite sample of caller-supplied broader results, not proof that the selector is sound. Complete and source identity are assertions made by the supplying report, not verified execution evidence. Rust records mean library tests only unless an explicit non-lib target is supplied, which requires separate scenario review; filters use Cargo substring matching. File selection is exact after path normalization. Browser/native failures require separate scenario review. Zero misses does not replace full gates or qualify future selections." };
  if (!report.qualified) return report;
  report.selectedFailures = []; report.misses = []; report.escalations = [];
  for (const failure of failures) {
    if (["browser", "native"].includes(failure.kind) || (failure.kind === "rust" && failure.target !== undefined && failure.target !== "lib")) { report.escalations.push(failure); continue; }
    const selected = failure.kind === "rust" ? selection.rust.some(target => target.package === failure.package && target.filters.some(filter => failure.test.includes(filter))) : selection[failure.kind].has(failure.path);
    (selected ? report.selectedFailures : report.misses).push(failure);
  }
  report.firstMissedRegression = report.misses[0] ?? null;
  report.status = report.misses.length ? "missed-regression" : report.escalations.length ? "requires-review" : "no-misses-observed";
  report.exitCode = report.misses.length ? 1 : report.escalations.length ? 2 : 0;
  return report;
}
export function formatReport(report) {
  const lines = [`Affected selection shadow audit: ${report.status}`, `Plan SHA: ${report.planSource || "unknown"}; broader SHA: ${report.resultsSource || "unknown"}`];
  if (!report.qualified) lines.push(...report.reasons, "Miss count: unknown (unqualified observation)");
  else {
    lines.push(`Selected failures: ${report.selectedFailures.length}; missed applicable failures: ${report.misses.length}; scenario escalations: ${report.escalations.length}`);
    for (const [label, records] of [["Miss", report.misses], ["Requires review", report.escalations]]) for (const failure of records) lines.push(`${label}: ${failure.kind} ${failure.package || ""} ${failure.path || failure.test}`.trim());
    if (report.firstMissedRegression) lines.push(`First missed regression (input order): ${JSON.stringify(report.firstMissedRegression)}`);
  }
  lines.push(report.limitations);
  return `${lines.join("\n")}\n`;
}
async function main(args) {
  const options = {};
  for (let index = 0; index < args.length; index++) {
    const option = args[index];
    if (option === "--json") { options.json = true; continue; }
    if (option === "--help") { options.help = true; continue; }
    if (!["--plan", "--results"].includes(option) || !args[index + 1] || args[index + 1].startsWith("--")) throw Error(`Invalid option or missing value: ${option}`);
    options[option.slice(2)] = args[++index];
  }
  if (options.help) {
    console.log("Usage: node scripts/validation/selection-audit.mjs --plan affected-plan.json --results broader-results.json [--json]\nResults: {source: <40-character SHA>, complete: true, failures: [{kind: rust|frontend|node|browser|native, package?, test?, path?, target?}], suites?: <scope evidence>}. Plan needs source SHA, dirty:false, no blockers and rust/frontend/node selections. Rust records mean library tests only; explicit non-lib targets require scenario review. Empty failures are valid only as a complete observation. Exit 0: no misses observed; 1: missed applicable regression; 2: unqualified, scenario review required, or invalid input. Full gates remain required.");
    return;
  }
  if (!options.plan || !options.results) throw Error("Invalid input: --plan and --results are required");
  const [plan, results] = await Promise.all([options.plan, options.results].map(async filename => JSON.parse(await readFile(filename, "utf8"))));
  const report = summarize(plan, results);
  process.stdout.write(options.json ? `${JSON.stringify(report, null, 2)}\n` : formatReport(report));
  process.exitCode = report.exitCode;
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 2; });
