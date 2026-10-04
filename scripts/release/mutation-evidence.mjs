import assert from "node:assert/strict";

export function parseSelectedMutants(output, status) {
  assert.equal(status, 0, "mutation list tool failed");
  // cargo-mutants 27.1.0 exits before emitting JSON for an empty selection.
  const selected = output.trim() === "" ? [] : JSON.parse(output);
  assert.ok(Array.isArray(selected), "invalid mutation list");
  return selected;
}

// The workflow pins cargo-mutants because its report schema is not a stable API.
export function validateMutationReport(selected, report, exitCode) {
  assert.ok(Number.isSafeInteger(selected) && selected >= 0, "invalid selected count");
  assert.ok([0, 2].includes(exitCode), `mutation tool exit indicates failure: ${exitCode}`);
  if (selected === 0) {
    assert.equal(report, null, "zero mutations must not reuse a stale report");
    assert.equal(exitCode, 0, "zero mutations require successful exit");
    return { selected: 0, caught: 0, missed: 0, unviable: 0, timeout: 0 };
  }
  assert.ok(report && Array.isArray(report.outcomes), "missing mutation report");
  assert.equal(report.cargo_mutants_version, "27.1.0", "unexpected mutation report version");
  assert.ok(report.end_time, "incomplete mutation report");
  assert.equal(report.total_mutants, selected, "selected/result count mismatch");
  const baseline = report.outcomes.filter((outcome) => outcome.scenario === "Baseline");
  assert.equal(baseline.length, 1, "missing or duplicate mutation baseline");
  assert.equal(baseline[0].summary, "Success", "mutation baseline failed");
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
