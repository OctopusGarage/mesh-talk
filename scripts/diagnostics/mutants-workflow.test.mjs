import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(new URL("../../.github/workflows/mutants.yml", import.meta.url), "utf8");
const wrapper = readFileSync(new URL("../release/run-mutants.mjs", import.meta.url), "utf8");

test("both mutation modes forward test threads to the test binary, not Cargo", () => {
  assert.equal([...workflow.matchAll(/run: node \.\.\/scripts\/release\/run-mutants\.mjs/g)].length, 2);
  assert.match(wrapper, /"--", "--", "--test-threads=2"/);
  assert.doesNotMatch(workflow, /continue-on-error: true/);
  assert.match(workflow, /cargo test --workspace -- --test-threads=2/);
  assert.match(workflow, /tool: cargo-mutants@27\.1\.0/);
});

test("mutation report upload includes the workspace-root output directory", () => {
  const upload = workflow.split(/^      - /m).find((step) => /name: Upload mutants report/.test(step));
  assert.ok(upload, "mutation report upload must be present");
  assert.match(upload, /^\s+(?:path: )?mutants\.out\/\s*$/m);
  assert.match(upload, /if: always\(\) && hashFiles\('mutants\.out\/\*\*'\) != ''/);
});
