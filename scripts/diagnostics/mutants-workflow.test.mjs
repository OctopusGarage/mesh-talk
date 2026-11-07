import { nextestProfileSection } from "./test-fixtures/nextest.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { mutationBudgets, requireMutationGate } from "../release/mutation-evidence.mjs";
const workflow = readFileSync(new URL("../../.github/workflows/mutants.yml", import.meta.url), "utf8");
const wrapper = readFileSync(new URL("../release/run-mutants.mjs", import.meta.url), "utf8");
const helper = readFileSync(new URL("../release/mutation-baseline.mjs", import.meta.url), "utf8");

test("main evidence retention and the one protected mutation name remain unchanged", () => {
  assert.match(workflow, /group: mutants-\$\{\{ github.event_name \}\}-/u);
  assert.match(workflow, /cancel-in-progress: \$\{\{ github.event_name == 'pull_request' \|\| github.event_name == 'merge_group' \}\}/u);
  assert.equal([...workflow.matchAll(/^    name: mutants$/gmu)].length, 1);
  assert.match(workflow, /name: Workspace mutation sample \(\$\{\{ matrix.worker \}\}\)/u);
  assert.match(workflow.split("  plan:\n")[0], /node --test[^\n]*scripts\/release\/mutation-process-owner\.test\.mjs/u, "the protected contract job must run real Linux cancellation controls");
});

test("deep selection precedes native work and dynamically admits every bounded chunk", () => {
  assert.match(workflow, /  plan:\n/u);
  assert.match(workflow, /--plan /u);
  assert.match(workflow, /matrix: \$\{\{ fromJSON\(needs.plan.outputs.matrix\) \}\}/u);
  assert.match(workflow, /max-parallel: 8/u);
  assert.match(workflow, /needs.plan.outputs.selected != '0'/u);
  assert.match(workflow, /needs: \[plan, baseline\]/u);
  assert.match(workflow, /if: github.event_name == 'schedule' \|\| github.event_name == 'workflow_dispatch'/u);
  assert.doesNotMatch(workflow, /MUTATION_WORKERS: ['"]4['"]|worker: \[0, 1, 2, 3\]/u);
});

test("trusted immutable producer artifacts carry their actual attempts through reruns", () => {
  for (const value of ["MUTATION_PLAN_DIGEST", "MUTATION_PLAN_ATTEMPT", "MUTATION_BASELINE_DIGEST", "MUTATION_BASELINE_ATTEMPT"]) assert.match(workflow, new RegExp(`${value}:`));
  assert.match(workflow, /artifact-ids: \$\{\{ needs.plan.outputs.artifact_id \}\}/u);
  assert.match(workflow, /artifact-ids: \$\{\{ needs.baseline.outputs.artifact_id \}\}/u);
  assert.match(workflow, /producer_attempt: \$\{\{ steps.baseline.outputs.producer_attempt \}\}/u);
  assert.doesNotMatch(helper, /receipt\.producerAttempt, attempt\(\)/u);
  assert.equal([...workflow.matchAll(/node-version: \$\{\{ needs.plan.outputs.node \}\}/gu)].length, 2, "baseline and workers must use the actual plan Node version");
  assert.match(workflow, /name: mutants-report-\$\{\{ matrix.worker \}\}-\$\{\{ github.run_attempt \}\}/u);
});

test("gate truth table rejects absent, failed, cancelled or unexpected deep evidence", () => {
  const statuses = ["success", "failure", "cancelled", "skipped", ""];
  for (const deep of [false, true]) for (const selected of ["0", "1", ""]) for (const contracts of statuses) for (const plan of statuses) for (const baseline of statuses) for (const workers of statuses) {
    const wanted = contracts === "success" && (deep ? plan === "success" && selected !== "" && baseline === (selected === "0" ? "skipped" : "success") && workers === baseline : plan === "skipped" && baseline === "skipped" && workers === "skipped");
    let accepted = false;
    try { requireMutationGate({ contracts, deep, plan, baseline, workers, selected }); accepted = true; } catch { /* Failure is the expected verdict for incomplete evidence. */ }
    assert.equal(accepted, wanted, JSON.stringify({ deep, selected, contracts, plan, baseline, workers }));
  }
  const gate = workflow.split("  mutation-gate:\n")[1];
  assert.match(gate, /needs: \[runner-contract, plan, baseline, mutants\]/u);
  assert.match(gate, /if: \$\{\{ !cancelled\(\) \}\}/u);
  assert.match(gate, /run-mutants.mjs --gate/u);
});

test("gate CLI consumes the actual dependency verdicts", () => {
  for (const [baseline, expected] of [["success", 0], ["failure", 1]]) {
    const result = spawnSync(process.execPath, [new URL("../release/run-mutants.mjs", import.meta.url).pathname, "--gate"], { env: { ...process.env, CONTRACTS: "success", DEEP: "true", PLAN: "success", BASELINE: baseline, SHARDS: "success", SELECTED: "1" } });
    assert.equal(result.status === 0, expected === 0);
  }
});

test("phase and job bounds cover every planned mutant without relying on averages", () => {
  const worker = workflow.split("  mutants:\n")[1].split("  mutation-gate:\n")[0];
  const producer = workflow.split("  baseline:\n")[1].split("  mutants:\n")[0];
  const minutes = source => Number(source.match(/timeout-minutes: (\d+)/u)[1]);
  assert.ok(minutes(worker) * 60 > mutationBudgets.maxMutants * (mutationBudgets.compileSeconds + mutationBudgets.testSeconds) + 900 + 120);
  assert.ok(minutes(producer) * 60 > mutationBudgets.compileSeconds + mutationBudgets.testSeconds + 900 + 120);
  const config = readFileSync(new URL("../../.config/nextest.toml", import.meta.url), "utf8");
  assert.match(nextestProfileSection(config, "mutants"), /global-timeout = "1800s"/u);
  assert.match(nextestProfileSection(config, "mutants"), /period = "60s", terminate-after = 15, grace-period = "5s"/u);
  assert.match(helper, /runCommand/u);
  assert.match(helper, /timeoutMs: seconds \* 1000/u);
  assert.match(helper, /--baseline", "skip/u);
  assert.match(wrapper, /mutationOptions\(shard\)/u);
  assert.equal(mutationBudgets.compileSeconds, 1800);
  assert.equal(mutationBudgets.testSeconds, 1860);
});

test("shared cache prefix matches the old key plus job and only the producer writes", () => {
  // Pinned rust-cache config.ts appends key then job, or uses shared-key instead.
  assert.equal(["mutants", "mutants"].join("-"), "mutants-mutants");
  const producer = workflow.split("  baseline:\n")[1].split("  mutants:\n")[0];
  const worker = workflow.split("  mutants:\n")[1].split("  mutation-gate:\n")[0];
  assert.match(producer, /shared-key: mutants-mutants/u);
  assert.match(producer, /save-if: \$\{\{ github.ref == 'refs\/heads\/main' \}\}/u);
  assert.match(worker, /key: mutants/u);
  assert.match(worker, /save-if: 'false'/u);
  assert.doesNotMatch(workflow, /cache-workspace-crates|prefix-key|CARGO_BUILD_JOBS:/u);
  assert.match(workflow, /CARGO_INCREMENTAL: '1'/u);
});
