import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { spawnSync } from "node:child_process";

const workflow = readFileSync(new URL("../../.github/workflows/mutants.yml", import.meta.url), "utf8");
const wrapper = readFileSync(new URL("../release/run-mutants.mjs", import.meta.url), "utf8");

test("the protected mutants check requires every mutation shard to succeed", () => {
  const gate = workflow.split("  mutation-gate:\n")[1];
  assert.ok(gate, "the sharded workflow must still report the protected mutants check");
  assert.match(gate, /name: mutants\n/);
  assert.match(gate, /needs: mutants\n/);
  assert.match(gate, /if: always\(\)/);
  assert.match(gate, /SHARDS: \$\{\{ needs.mutants.result \}\}/);
  const script = gate.split("        run: |\n")[1].replace(/^          /gm, "");
  for (const result of ["success", "failure", "cancelled", "skipped", ""]) {
    const run = spawnSync("bash", ["-e", "-c", script], {
      encoding: "utf8", env: { ...process.env, SHARDS: result },
    });
    assert.equal(run.status === 0, result === "success", result);
  }
});

test("both mutation modes forward test threads to the test binary, not Cargo", () => {
  assert.equal([...workflow.matchAll(/run: node \.\.\/scripts\/release\/run-mutants\.mjs/g)].length, 2);
  assert.match(wrapper, /"--", "--", "--test-threads=2"/);
  assert.doesNotMatch(workflow, /continue-on-error: true/);
  assert.doesNotMatch(workflow, /cargo test --workspace/);
  assert.doesNotMatch(wrapper, /"--baseline", "skip"/);
  assert.match(workflow, /tool: cargo-mutants@27\.1\.0/);
  assert.match(wrapper, /const shard = mutationShard\(Number\(process\.env\.GITHUB_RUN_NUMBER/);
  assert.match(wrapper, /"--shard", shard/);
  assert.match(wrapper, /"--sharding", "round-robin"/);
  assert.match(wrapper, /"--timeout", "240"/);
});

test("mutation report upload includes the workspace-root output directory", () => {
  const upload = workflow.split(/^      - /m).find((step) => /name: Upload mutants report/.test(step));
  assert.ok(upload, "mutation report upload must be present");
  assert.match(upload, /^\s+(?:path: )?mutants\.out\/\s*$/m);
  assert.match(upload, /if: always\(\) && hashFiles\('mutants\.out\/\*\*'\) != ''/);
});

test("mutation builds reuse one incremental tree per matrix runner", () => {
  assert.match(workflow, /worker: \[0, 1, 2, 3\]/);
  assert.match(workflow, /fail-fast: false/);
  assert.match(workflow, /CARGO_INCREMENTAL: ['"]1['"]/);
  assert.match(workflow, /MUTATION_WORKER: \$\{\{ matrix.worker \}\}/);
  assert.match(workflow, /MUTATION_WORKERS: ['"]4['"]/);
  assert.match(workflow, /workspaces: ['"]\.['"]/);
  assert.match(wrapper, /"--in-place"/);
  assert.match(wrapper, /"--profile", "mutants"/);
  assert.doesNotMatch(wrapper, /"(?:-j|--jobs)"/);
  assert.match(workflow, /name: mutants-report-\$\{\{ matrix.worker \}\}/);
});
