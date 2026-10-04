import { test } from "node:test";
import assert from "node:assert/strict";

test("backend evidence rejects successful Cargo invocations that ran zero tests", async () => {
  const { validateBackendOutput } = await import("../evals/backend-runner.mjs");
  assert.throws(() => validateBackendOutput("test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out", ["actual_flow"]), /missing|zero|count/i);
});
test("backend evidence requires every expected scenario exactly once, not ignored", async () => {
  const { validateBackendOutput } = await import("../evals/backend-runner.mjs");
  const valid = "test actual_flow ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
  assert.deepEqual(validateBackendOutput(valid, ["actual_flow"]), ["actual_flow"]);
  for (const output of [valid.replace("... ok", "... ignored"), valid + "\ntest actual_flow ... ok", valid.replace("0 failed", "1 failed"), valid.replace("1 passed", "2 passed")]) assert.throws(() => validateBackendOutput(output, ["actual_flow"]));
});
test("backend evaluates every independent suite after a failure and retains a failing verdict", async () => {
  const { BACKEND_SUITES, evaluateBackendSuites } = await import("../evals/backend-runner.mjs");
  const attempted = [], recorded = {};
  const failures = await evaluateBackendSuites(async target => {
    attempted.push(target);
    const expected = BACKEND_SUITES[target];
    if (target === "post_office_offline") return { code: 101, output: "relay regression", signal: null };
    return { code: 0, signal: null, output: expected.map(name => `test ${name} ... ok`).join("\n") + `\ntest result: ok. ${expected.length} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out` };
  }, async (target, record) => { recorded[target] = record; });
  assert.deepEqual(attempted, Object.keys(BACKEND_SUITES));
  assert.equal(failures.length, 1);
  assert.equal(recorded.post_office_offline.passed, false);
  assert.match(recorded.post_office_offline.failure, /failed/);
  assert.equal(recorded.channel_and_file_cli.passed, true);
  assert.equal(recorded.channel_and_file_cli.scenarios.length, 2);
});
