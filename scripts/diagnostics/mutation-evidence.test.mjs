import assert from "node:assert/strict";
import test from "node:test";
import { parseSelectedMutants, validateMutationReport } from "../release/mutation-evidence.mjs";

const outcome = (summary) => ({ scenario: { Mutant: {} }, summary });
const report = {
  total_mutants: 3, caught: 1, missed: 1, unviable: 1, timeout: 0, success: 0,
  end_time: "2026-10-04T00:00:00Z", cargo_mutants_version: "27.1.0",
  outcomes: [{ scenario: "Baseline", summary: "Success" }, ...["CaughtMutant", "MissedMutant", "Unviable"].map(outcome)],
};

test("pinned tool's empty successful listing explicitly represents zero mutants", () => {
  assert.deepEqual(parseSelectedMutants("", 0), []);
  assert.deepEqual(parseSelectedMutants("[]\n", 0), []);
  assert.deepEqual(parseSelectedMutants('[{"name":"mutation"}]', 0), [{ name: "mutation" }]);
  assert.throws(() => parseSelectedMutants("", 1), /list/);
  assert.throws(() => parseSelectedMutants("{}", 0), /list/);
  assert.throws(() => parseSelectedMutants("not-json", 0));
});

test("valid survivors remain advisory and unviable mutants are not counted as caught", () => {
  assert.deepEqual(validateMutationReport(3, report, 2), { selected: 3, caught: 1, missed: 1, unviable: 1, timeout: 0 });
});

test("zero selected mutations require successful tooling and no stale report", () => {
  assert.deepEqual(validateMutationReport(0, null, 0), { selected: 0, caught: 0, missed: 0, unviable: 0, timeout: 0 });
  assert.throws(() => validateMutationReport(0, report, 0), /report/);
  assert.throws(() => validateMutationReport(0, null, 1), /exit/);
});

test("missing, incomplete and inconsistent reports fail even when the tool exits zero", () => {
  assert.throws(() => validateMutationReport(3, null, 0), /report/);
  assert.throws(() => validateMutationReport(3, { ...report, end_time: null }, 2), /incomplete/);
  assert.throws(() => validateMutationReport(4, report, 2), /count/);
  assert.throws(() => validateMutationReport(3, { ...report, caught: 2 }, 2), /count/);
  assert.throws(() => validateMutationReport(3, { ...report, outcomes: report.outcomes.slice(1) }, 2), /baseline/);
  assert.throws(() => validateMutationReport(3, { ...report, outcomes: [{ scenario: "Baseline", summary: "Failure" }, ...report.outcomes.slice(1)] }, 2), /baseline/);
  assert.throws(() => validateMutationReport(3, report, 0), /exit/);
});

test("usage, baseline, diff, timeout and internal failures never become advisory successes", () => {
  for (const code of [1, 3, 4, 5, 6, 70, 137, null]) assert.throws(() => validateMutationReport(3, report, code), /exit/);
});
