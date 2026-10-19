import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = name => readFileSync(new URL(`../../.github/workflows/${name}.yml`, import.meta.url), "utf8");

for (const name of ["ci", "check-health", "e2e-backend", "e2e-ui", "sanitizers", "codeql", "gitleaks", "dependency-review", "mutants"]) {
  test(`${name} reports required checks for merge candidates`, () => {
    const workflow = read(name);
    assert.match(workflow, /^  merge_group:\n    types: \[checks_requested\]/m);
    assert.match(workflow, /^  pull_request:/m);
    if (name !== "dependency-review") assert.match(workflow, /^  push:/m);
  });
}

for (const name of ["sanitizers", "codeql", "gitleaks", "dependency-review"]) {
  test(`${name} cancels superseded candidates without canceling publication runs`, () => {
    const workflow = read(name);
    assert.match(workflow, /group: \$\{\{ github.workflow \}\}-\$\{\{ github.event_name \}\}-\$\{\{ github.event.pull_request.number \|\| github.ref \}\}/);
    assert.match(workflow, /cancel-in-progress: \$\{\{ github.event_name == 'pull_request' \|\| github.event_name == 'merge_group' \}\}/);
  });
}

test("dependency review compares merge-group base and head without requiring a PR payload", () => {
  const workflow = read("dependency-review");
  assert.match(workflow, /base-ref: \$\{\{ github.event.merge_group.base_sha \|\| github.event.pull_request.base.sha \}\}/);
  assert.match(workflow, /head-ref: \$\{\{ github.event.merge_group.head_sha \|\| github.event.pull_request.head.sha \}\}/);
  assert.match(workflow, /comment-summary-in-pr: \$\{\{ github.event_name == 'pull_request' && 'on-failure' \|\| 'never' \}\}/);
  assert.doesNotMatch(workflow, /warn-only: true|if:.*pull_request/);
});
