import assert from "node:assert/strict";
import test from "node:test";
import { mutationShard, parseSelectedMutants, validateMutationReport } from "../release/mutation-evidence.mjs";

test("mutation shard rotates across workflow runs and stays within 256 shards", () => {
  assert.equal(mutationShard(1), "0/256");
  assert.equal(mutationShard(2), "1/256");
  assert.equal(mutationShard(256), "255/256");
  assert.equal(mutationShard(257), "0/256");
  assert.throws(() => mutationShard(0), /invalid workflow run number/);
});

test("parallel shards partition exactly the original rotating sample", () => {
  for (const run of [1, 256, 257, 472]) {
    const original = (run - 1) % 256;
    const shards = Array.from({ length: 4 }, (_, worker) => mutationShard(run, worker, 4));
    assert.equal(new Set(shards).size, 4);
    for (let mutant = 0; mutant < 9000; mutant++) {
      const matches = shards.filter((shard) => {
        const [index, count] = shard.split("/").map(Number);
        return mutant % count === index;
      }).length;
      assert.equal(matches, mutant % 256 === original ? 1 : 0);
    }
  }
  assert.equal(mutationShard(472, 0, 4), "215/1024");
  assert.equal(mutationShard(472, 3, 4), "983/1024");
  for (const [worker, count] of [[-1, 4], [4, 4], [0.5, 4], [0, 0], [0, 1.5]]) {
    assert.throws(() => mutationShard(1, worker, count), /invalid mutation worker/);
  }
});

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

test("bounded plans retain the exact rotating sample and reject tampering", async () => {
  const { createMutationPlan, verifyMutationPlan } = await import("../release/mutation-evidence.mjs");
  const selected = Array.from({ length: 129 }, (_, index) => ({ name: `mutant-${index}` }));
  const plan = createMutationPlan(472, selected);
  assert.equal(plan.workers.length, 33);
  assert.ok(plan.workers.every(worker => worker.selected.length <= 4));
  assert.deepEqual(plan.workers.flatMap(worker => worker.selected).sort((a, b) => a.name.localeCompare(b.name)), [...selected].sort((a, b) => a.name.localeCompare(b.name)));
  verifyMutationPlan(plan);
  assert.throws(() => verifyMutationPlan({ ...plan, workers: plan.workers.slice(1) }), /plan/);
  assert.throws(() => createMutationPlan(1, [selected[0], selected[0]]), /duplicate/i);
  assert.throws(() => createMutationPlan(1, [{ name: "same", diff: "first" }, { name: "same", diff: "changed rendering" }]), /duplicate/i);
  assert.throws(() => createMutationPlan(1, Array.from({ length: 1025 }, (_, index) => ({ index }))), /capacity/);
  assert.deepEqual(createMutationPlan(1, []).workers, []);
});

test("external reports require verified evidence and zero manufactured baseline records", async () => {
  const { evidenceDigest, verifyBaselineReceipt } = await import("../release/mutation-evidence.mjs");
  const receipt = { schema: 1, role: "mutation-baseline", identity: { source: "a", runId: "1" }, producerAttempt: "1", planDigest: "p", tests: 4, passed: 3, ignored: 1, files: { inventory: "i", execution: "e" } };
  const expected = { identity: receipt.identity, producerAttempt: "1", planDigest: "p", receiptDigest: evidenceDigest(receipt) };
  verifyBaselineReceipt(receipt, expected);
  const external = { ...report, outcomes: report.outcomes.slice(1) };
  assert.deepEqual(validateMutationReport(3, external, 2, { externalBaseline: receipt, expectedBaseline: expected }), { selected: 3, caught: 1, missed: 1, unviable: 1, timeout: 0 });
  assert.throws(() => validateMutationReport(3, external, 2), /baseline/);
  assert.throws(() => validateMutationReport(3, report, 2, { externalBaseline: receipt, expectedBaseline: expected }), /baseline/);
  for (const wrong of [{ ...expected, producerAttempt: "2" }, { ...expected, identity: { source: "b" } }, { ...expected, receiptDigest: "wrong" }]) assert.throws(() => verifyBaselineReceipt(receipt, wrong));
  assert.throws(() => verifyBaselineReceipt({ ...receipt, passed: 2 }, { ...expected, receiptDigest: evidenceDigest({ ...receipt, passed: 2 }) }), /incomplete/);
});

test("raw pinned outcomes omit only rendered diff and must retain every selected identity", async () => {
  const { verifyMutationOutcomes } = await import("../release/mutation-evidence.mjs");
  const selected = [{ name: "one", file: "src/lib.rs", replacement: "true", diff: "rendered patch" }, { name: "two", file: "src/lib.rs", replacement: "false", diff: "second patch" }];
  const outcomes = selected.map(({ diff: _diff, ...identity }) => ({ scenario: { Mutant: identity }, summary: "CaughtMutant" }));
  verifyMutationOutcomes(selected, { outcomes });
  assert.throws(() => verifyMutationOutcomes(selected, { outcomes: [outcomes[0], outcomes[0]] }), /omission or duplicate/);
  assert.throws(() => verifyMutationOutcomes(selected, { outcomes: outcomes.slice(1) }), /omission or duplicate/);
  assert.throws(() => verifyMutationOutcomes(selected, { outcomes: [{ scenario: { Mutant: { ...outcomes[0].scenario.Mutant, replacement: "0" } } }, outcomes[1]] }), /omission or duplicate/);
});
