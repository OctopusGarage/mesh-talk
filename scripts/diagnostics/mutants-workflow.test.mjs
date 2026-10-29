import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { spawnSync } from "node:child_process";

const workflow = readFileSync(new URL("../../.github/workflows/mutants.yml", import.meta.url), "utf8");
const wrapper = readFileSync(new URL("../release/run-mutants.mjs", import.meta.url), "utf8");

test("main pushes cannot cancel scheduled deep assurance", () => {
  assert.match(workflow, /group: mutants-\$\{\{ github.event_name \}\}-/);
  assert.match(workflow, /cancel-in-progress: \$\{\{ github.event_name == 'pull_request' \|\| github.event_name == 'merge_group' \|\| github.event_name == 'push' \}\}/);
});

test("skipped deep jobs cannot impersonate the protected mutation gate", () => {
  const deep = workflow.split("  mutants:\n")[1].split("  mutation-gate:\n")[0];
  const name = deep.match(/^    name: (.+)$/m)?.[1];
  assert.ok(name, "deep jobs need an explicit name even when skipped before matrix expansion");
  const skippedName = name.replace(/\$\{\{ matrix\.worker \}\}/g, "").trim();
  assert.notEqual(skippedName, "mutants", "only the final aggregate owns the protected name");
  assert.equal([...workflow.matchAll(/^    name: mutants$/gm)].length, 1);
});

test("mutation selection precedes expensive setup and zero selections skip only execution work", () => {
  const selection = workflow.indexOf("name: Select mutation sample");
  assert.ok(selection > 0);
  for (const name of ["Install Tauri system dependencies", "Build frontend", "Cache mutation build"]) {
    const block = workflow.split(/^      - /m).find(value => value.startsWith(`name: ${name}\n`));
    assert.ok(block, name);
    assert.ok(workflow.indexOf(`name: ${name}`) > selection, name);
    assert.match(block, /if: steps.selection.outputs.selected != '0'/);
  }
  assert.match(workflow, /--list-only\n/);
  for (const name of ["Mutants (workspace sample)"]) {
    const block = workflow.split(/^      - /m).find(value => value.startsWith(`name: ${name}\n`));
    assert.match(block, /steps.selection.outputs.selected != '0'/);
  }
});

test("the protected mutation gate requires runner contracts and every selected deep shard", () => {
  const gate = workflow.split("  mutation-gate:\n")[1];
  assert.ok(gate);
  assert.match(gate, /name: mutants\n/);
  assert.match(gate, /needs: \[runner-contract, mutants\]/);
  assert.match(gate, /if: always\(\)/);
  const script = gate.split("        run: |\n")[1].replace(/^          /gm, "");
  for (const deep of ["true", "false"]) {
    for (const contracts of ["success", "failure", "cancelled", "skipped", ""]) {
      for (const shards of ["success", "failure", "cancelled", "skipped", ""]) {
        const run = spawnSync("bash", ["-e", "-c", script], {
          encoding: "utf8", env: { ...process.env, CONTRACTS: contracts, SHARDS: shards, DEEP: deep },
        });
        assert.equal(run.status === 0, contracts === "success" && shards === (deep === "true" ? "success" : "skipped"), `${deep}/${contracts}/${shards}`);
      }
    }
  }
  const deep = workflow.split("  mutants:\n")[1].split("  mutation-gate:\n")[0];
  assert.match(deep, /if: github.event_name == 'schedule' \|\| github.event_name == 'workflow_dispatch'/);
  const contracts = workflow.split("  runner-contract:\n")[1].split("  mutants:\n")[0];
  assert.match(contracts, /scripts\/release\/run-mutants.test.mjs/);
});

test("deep mutation samples use process-isolated tests with bounded execution", () => {
  assert.equal([...workflow.matchAll(/run: node \.\.\/scripts\/release\/run-mutants\.mjs$/gm)].length, 1);
  assert.match(wrapper, /"--test-tool", "nextest"/);
  assert.match(wrapper, /"--cargo-test-arg=--profile=mutants"/);
  assert.match(wrapper, /"--", "--test-threads=4"/);
  assert.doesNotMatch(workflow, /continue-on-error: true/);
  assert.doesNotMatch(workflow, /cargo test --workspace/);
  assert.doesNotMatch(wrapper, /"--baseline", "skip"/);
  assert.match(workflow, /tool: cargo-mutants@27\.1\.0/);
  assert.match(workflow, /cargo-nextest@0\.9\.148/);
  assert.match(wrapper, /const shard = mutationShard\(Number\(process\.env\.GITHUB_RUN_NUMBER/);
  assert.match(wrapper, /"--shard", shard/);
  assert.match(wrapper, /"--sharding", "round-robin"/);
  assert.match(wrapper, /"--timeout", "360"/);
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

test("mutation job budget covers the full workspace sample with isolated hang detection", () => {
  // Run 40187525604 selected 17 mutants in a shard. Builds reached 130s;
  // budget the full workspace, not just the smaller package baseline.
  const selected = 17;
  const buildSeconds = 130;
  const suiteSeconds = Number(wrapper.match(/"--timeout", "(\d+)"/)[1]);
  // Run 40922461468: core work alone occupied 888.73 process-seconds;
  // completed host tests add 75 seconds before the remaining workspace tests.
  // This lower bound already exceeds the old 240s limit at four processes.
  const observedWorkSeconds = 888.73 + 75;
  assert.ok(suiteSeconds >= observedWorkSeconds / 4 * 1.25,
    "suite budget must include the actual workspace inventory and runner variance");
  const nextest = readFileSync(new URL("../../.config/nextest.toml", import.meta.url), "utf8");
  assert.match(nextest, /period = "60s", terminate-after = 1, grace-period = "5s"/);
  assert.match(nextest, /test-threads = 4/);
  const setupSeconds = 6 * 60;
  const jobMinutes = Number(workflow.split("  mutants:\n")[1].match(/timeout-minutes: (\d+)/)[1]);
  assert.ok(jobMinutes * 60 >= setupSeconds + (selected + 1) * (buildSeconds + suiteSeconds),
    "job timeout must allow the baseline and complete sample to finish");
});
