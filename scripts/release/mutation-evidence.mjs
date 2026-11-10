import assert from "node:assert/strict";
import { createHash } from "node:crypto";

export function mutationShard(runNumber, worker = 0, workers = 1) {
  assert.ok(Number.isSafeInteger(runNumber) && runNumber > 0, "invalid workflow run number");
  assert.ok(Number.isSafeInteger(workers) && workers > 0 && Number.isSafeInteger(256 * workers), "invalid mutation worker count");
  assert.ok(Number.isSafeInteger(worker) && worker >= 0 && worker < workers, "invalid mutation worker index");
  // Subdivide the original round-robin sample without increasing or shrinking it.
  return `${(runNumber - 1) % 256 + 256 * worker}/${256 * workers}`;
}

export function parseSelectedMutants(output, status) {
  assert.equal(status, 0, "mutation list tool failed");
  // cargo-mutants 27.1.0 exits before emitting JSON for an empty selection.
  const selected = output.trim() === "" ? [] : JSON.parse(output);
  assert.ok(Array.isArray(selected), "invalid mutation list");
  return selected;
}

export const mutationToolVersion = "27.1.0";

// The workflow pins cargo-mutants because its report schema is not a stable API.
export function validateMutationReport(selected, report, exitCode, { externalBaseline, expectedBaseline } = {}) {
  assert.ok(Number.isSafeInteger(selected) && selected >= 0, "invalid selected count");
  assert.ok([0, 2].includes(exitCode), `mutation tool exit indicates failure: ${exitCode}`);
  if (selected === 0) {
    assert.equal(report, null, "zero mutations must not reuse a stale report");
    assert.equal(externalBaseline, undefined, "zero mutations must not reuse a baseline");
    assert.equal(exitCode, 0, "zero mutations require successful exit");
    return { selected: 0, caught: 0, missed: 0, unviable: 0, timeout: 0 };
  }
  assert.ok(report && Array.isArray(report.outcomes), "missing mutation report");
  assert.equal(report.cargo_mutants_version, mutationToolVersion, "unexpected mutation report version");
  assert.ok(report.end_time, "incomplete mutation report");
  assert.equal(report.total_mutants, selected, "selected/result count mismatch");
  const baseline = report.outcomes.filter((outcome) => outcome.scenario === "Baseline");
  if (externalBaseline !== undefined) {
    verifyBaselineReceipt(externalBaseline, expectedBaseline);
    assert.equal(baseline.length, 0, "external mode forbids inline or synthetic baseline records");
  } else {
    assert.equal(baseline.length, 1, "missing or duplicate mutation baseline");
    assert.equal(baseline[0].summary, "Success", "mutation baseline failed");
  }
  const counts = { CaughtMutant: 0, MissedMutant: 0, Unviable: 0, Timeout: 0 };
  for (const outcome of report.outcomes.filter((value) => value.scenario !== "Baseline")) {
    assert.ok(outcome.scenario?.Mutant && Object.hasOwn(counts, outcome.summary), "invalid mutation outcome");
    counts[outcome.summary]++;
  }
  assert.equal(Object.values(counts).reduce((sum, value) => sum + value, 0), selected, "outcome count mismatch");
  for (const [field, summary] of [["caught", "CaughtMutant"], ["missed", "MissedMutant"], ["unviable", "Unviable"], ["timeout", "Timeout"]]) {
    assert.equal(report[field], counts[summary], `${field} count mismatch`);
  }
  assert.equal(report.success, 0, "mutation tests must not be check-only");
  assert.equal(report.timeout, 0, "mutation timeout is inconclusive");
  assert.equal(exitCode, report.missed > 0 ? 2 : 0, "mutation report/exit mismatch");
  return { selected, caught: report.caught, missed: report.missed, unviable: report.unviable, timeout: report.timeout };
}

const stable = value => Array.isArray(value) ? value.map(stable) : value && typeof value === "object" ? Object.fromEntries(Object.keys(value).sort().map(key => [key, stable(value[key])])) : value;
export const evidenceDigest = value => createHash("sha256").update(JSON.stringify(stable(value))).digest("hex");
const mutantDigest = item => { const { diff: _diff, ...identity } = item; return evidenceDigest(identity); };
export const mutationBudgets = Object.freeze({ maxMutants: 4, compileSeconds: 1800, testSeconds: 1860, baselineSeconds: 1800, maxWorkers: 256 });

export function createMutationPlan(runNumber, selected) {
  mutationShard(runNumber);
  assert.ok(Array.isArray(selected), "invalid mutation plan selection");
  assert.ok(selected.every(item => item && typeof item === "object" && !Array.isArray(item)), "invalid mutation identity");
  assert.equal(new Set(selected.map(mutantDigest)).size, selected.length, "duplicate mutation selection");
  const count = Math.ceil(selected.length / mutationBudgets.maxMutants);
  assert.ok(count <= mutationBudgets.maxWorkers, "mutation plan exceeds matrix capacity");
  return { schema: 1, runNumber, selected, workers: Array.from({ length: count }, (_, worker) => ({ worker, shard: mutationShard(runNumber, worker, count), selected: selected.filter((_, index) => index % count === worker) })) };
}

export function verifyMutationPlan(plan) {
  assert.ok(plan?.schema === 1, "invalid mutation plan schema");
  assert.deepEqual(plan, createMutationPlan(plan.runNumber, plan.selected), "mutation plan drift or omission");
  return plan;
}

export function verifyBaselineReceipt(receipt, expected) {
  assert.ok(expected?.identity && expected.receiptDigest && expected.producerAttempt && expected.planDigest, "trusted baseline identity is required");
  assert.equal(evidenceDigest(receipt), expected.receiptDigest, "baseline receipt digest mismatch");
  assert.ok(receipt?.schema === 1 && receipt.role === "mutation-baseline", "invalid baseline receipt");
  assert.deepEqual(receipt.identity, expected.identity, "baseline source/tool/config identity mismatch");
  assert.equal(receipt.producerAttempt, expected.producerAttempt, "baseline producer attempt mismatch");
  assert.equal(receipt.planDigest, expected.planDigest, "baseline plan mismatch");
  assert.ok(Number.isSafeInteger(receipt.tests) && receipt.tests > 0 && Number.isSafeInteger(receipt.ignored) && receipt.ignored >= 0 && receipt.ignored < receipt.tests, "invalid baseline inventory");
  assert.equal(receipt.passed + receipt.ignored, receipt.tests, "baseline execution incomplete");
  assert.ok(receipt.files?.inventory && receipt.files.execution, "baseline raw evidence missing");
  return receipt;
}

// The pinned listing includes rendered diff text; raw Scenario::Mutant omits it.
export function verifyMutationOutcomes(selected, report) {
  assert.deepEqual(report.outcomes.filter(item => item.scenario !== "Baseline").map(item => mutantDigest(item.scenario.Mutant)).sort(), selected.map(mutantDigest).sort(), "mutation outcome omission or duplicate");
}

export function requireMutationGate({ contracts, deep, plan, baseline, workers, selected }) {
  assert.equal(contracts, "success", "mutation runner contracts failed or missing");
  if (!deep) {
    assert.deepEqual([plan, baseline, workers], ["skipped", "skipped", "skipped"], "unexpected deep work on contract-only event");
    return;
  }
  assert.equal(plan, "success", "mutation plan failed or missing");
  assert.ok(typeof selected === "string" && /^(0|[1-9]\d*)$/u.test(selected), "missing selected mutation count");
  const wanted = selected === "0" ? "skipped" : "success";
  assert.deepEqual([baseline, workers], [wanted, wanted], "mutation baseline or workers failed, missing or unexpected");
}
