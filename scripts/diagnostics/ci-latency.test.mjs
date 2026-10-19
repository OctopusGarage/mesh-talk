import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { spawnSync } from "node:child_process";

const read = name => readFileSync(new URL(`../../.github/workflows/${name}`, import.meta.url), "utf8");
const step = (source, name) => source.split(/^      - /m).find(value => value.startsWith(`name: ${name}\n`));

test("CI caches the workspace output and retains native lint and build coverage", () => {
  const ci = read("ci.yml");
  assert.match(ci, /workspaces: ['"]?\. -> target/);
  assert.doesNotMatch(step(ci, "Clippy"), /if:/);
  assert.match(step(ci, "Build workspace"), /cargo build --workspace --locked/);
  assert.doesNotMatch(step(ci, "Build workspace"), /if:/);
});

test("health owns fast checks without repeating Rust or browser suites", () => {
  const health = read("check-health.yml");
  assert.doesNotMatch(health, /bash scripts\/check-health\.sh|cargo (?:build|clippy|test)|npx playwright|src-tauri\/target/);
  for (const command of ["npm run format:check", "npm run lint", "npm run typecheck", "audit-ci", "cargo fmt --all -- --check", "cargo deny check", "cargo machete", "shellcheck", "actionlint", "typos"]) assert.ok(health.includes(command), command);
  assert.match(health, /matrix:\n        os: \[ubuntu-latest, macos-latest, windows-latest\]/);
  const ci = read("ci.yml");
  assert.match(step(ci, "Build retention safeguards (all platforms)"), /python3 -m unittest discover -s scripts -p test_build_retention.py -v/);
  for (const command of ["npm run lint", "cargo fmt", "cargo deny", "cargo machete", "shellcheck", "actionlint", "crate-ci/typos"]) assert.ok(!ci.includes(command), `duplicate ${command}`);
});

test("full browser coverage remains on every desktop OS with bounded shards", () => {
  const ui = read("e2e-ui.yml");
  const baseline = ui.split("  e2e:\n")[1].split(/^  [a-z][\w-]*:\n/m)[0];
  assert.match(baseline, /os: \[ubuntu-24.04, macos-latest, windows-latest\]/);
  assert.match(baseline, /shard: \[1, 2, 3, 4\]/);
  assert.match(baseline, /fail-fast: false/);
  assert.match(step(ui, "Run UI E2E"), /--workers=1 --retries=0 --shard=\$\{\{ matrix.shard \}\}\/4/);
  assert.match(baseline, /name: playwright-ui-\$\{\{ matrix.os \}\}-\$\{\{ matrix.shard \}\}-\$\{\{ github.run_attempt \}\}/);
  assert.match(baseline, /if-no-files-found: error/);
  assert.doesNotMatch(baseline, /test:coverage|codecov/);
});

test("the stable UI gate requires coverage merged from all Linux shards", () => {
  const ui = read("e2e-ui.yml");
  const coverage = ui.split("  coverage:\n")[1]?.split("  lite:\n")[0];
  assert.ok(coverage);
  assert.match(coverage, /needs: \[e2e, unit-coverage\]/);
  assert.match(coverage, /actions\/download-artifact@/);
  assert.match(coverage, /collect-ui-coverage.mjs/);
  assert.match(coverage, /merge-e2e-coverage.mjs/);
  assert.match(ui, /needs: \[e2e, coverage, reports, lite, portable, firefox-extended\]/);
  assert.match(ui, /COVERAGE: \$\{\{ needs.coverage.result \}\}/);
  assert.match(ui, /test "\$COVERAGE" = success/);
});

function runGate(file, name, env) {
  const gate = step(read(file), name);
  assert.ok(gate, `Missing gate: ${name}`);
  const script = gate.split("        run: |\n")[1].replace(/^          /gm, "");
  return spawnSync("bash", ["-e", "-c", script], { encoding: "utf8", env: { ...process.env, ...env } });
}

test("every protected health platform check requires both source and platform results", () => {
  const health = read("check-health.yml");
  assert.match(health, /needs: \[source, platform\]/);
  assert.match(health, /if: always\(\)/);
  for (const source of ["success", "failure", "cancelled", "skipped"]) {
    for (const platform of ["success", "failure", "cancelled", "skipped"]) {
      const result = runGate("check-health.yml", "Require complete health checks", { SOURCE: source, PLATFORM: platform });
      assert.equal(result.status === 0, source === "success" && platform === "success", `${source}, ${platform}`);
    }
  }
});

test("the final UI gate rejects failed, cancelled or missing required jobs", () => {
  const env = { BASELINE: "success", COVERAGE: "success", REPORTS: "success", LITE: "success", PORTABLE: "success", FIREFOX: "skipped", EXTENDED: "false" };
  assert.equal(runGate("e2e-ui.yml", "Require complete selected UI matrices", env).status, 0);
  for (const job of ["BASELINE", "COVERAGE", "REPORTS", "LITE", "PORTABLE"]) {
    for (const result of ["failure", "cancelled", "skipped", ""]) assert.notEqual(runGate("e2e-ui.yml", "Require complete selected UI matrices", { ...env, [job]: result }).status, 0, `${job}=${result}`);
  }
  assert.notEqual(runGate("e2e-ui.yml", "Require complete selected UI matrices", { ...env, EXTENDED: "true" }).status, 0);
  assert.equal(runGate("e2e-ui.yml", "Require complete selected UI matrices", { ...env, EXTENDED: "true", FIREFOX: "success" }).status, 0);
});

test("the protected Full Linux UI regression name remains a fail-closed check", () => {
  const coverage = read("e2e-ui.yml").split("  coverage:\n")[1]?.split("  reports:\n")[0];
  assert.match(coverage, /name: Full Linux UI regression/);
  assert.match(coverage, /if: always\(\)/);
  for (const result of ["success", "failure", "cancelled", "skipped"]) {
    assert.equal(runGate("e2e-ui.yml", "Require all browser shards", { BASELINE: result, UNIT_COVERAGE: "success" }).status === 0, result === "success");
  }
});


test("UI coverage consumers share one same-workflow Linux unit run", () => {
  const ui = read("e2e-ui.yml");
  assert.equal((ui.match(/run: npm run test:coverage/g) || []).length, 1);
  const producer = ui.split("  unit-coverage:\n")[1]?.split("  coverage:\n")[0];
  assert.ok(producer);
  assert.match(producer, /runs-on: ubuntu-24.04/);
  assert.match(producer, /unit-coverage-artifact.mjs seal/);
  assert.match(producer, /name: ui-unit-coverage-\$\{\{ github.sha \}\}-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}/);
  for (const job of ["coverage", "lite"]) {
    const consumer = ui.split(`  ${job}:\n`)[1].split(/^  [a-z][\w-]*:\n/m)[0];
    assert.match(consumer, /needs:.*unit-coverage/);
    assert.match(consumer, /unit-coverage-artifact.mjs restore/);
    assert.match(consumer, /actions\/download-artifact@/);
    assert.doesNotMatch(consumer, /run-id:|github-token:/);
  }
});

test("CI runs release workflow diagnostics only through the shared glob", () => {
  const ci = read("ci.yml");
  assert.match(ci, /node --test scripts\/diagnostics\/\*.test.mjs/);
  assert.doesNotMatch(ci, /node --test scripts\/diagnostics\/release-workflow.test.mjs/);
});
