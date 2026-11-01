import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { spawnSync } from "node:child_process";

const read = name => readFileSync(new URL(`../../.github/workflows/${name}`, import.meta.url), "utf8");
const step = (source, name) => source.split(/^      - /m).find(value => value.startsWith(`name: ${name}\n`));

test("verification Rust caches restore on PRs and publish reusable main caches", () => {
  for (const file of ["ci.yml", "windows-workspace.yml", "e2e-backend.yml", "hidden-contacts-native.yml", "sanitizers.yml", "mutants.yml"]) {
    const cache = read(file).split(/^      - /m).find(value => value.includes("uses: Swatinem/rust-cache@v2"));
    assert.ok(cache, file);
    assert.match(cache, /save-if: \$\{\{ github.ref == 'refs\/heads\/main' \}\}/, file);
    assert.doesNotMatch(cache, /^        if:.*github.ref/m, "PRs still restore compatible caches");
  }
});

test("portable diagnostics declare the Rust toolchain needed by their real Cargo fixture", () => {
  const platform = read("check-health.yml").split("  platform:\n")[1].split("  source:\n")[0];
  const toolchain = platform.indexOf("uses: dtolnay/rust-toolchain@stable");
  assert.ok(toolchain >= 0 && toolchain < platform.indexOf("node --test scripts/diagnostics/*.test.mjs"));
});

test("portable validation runs beside native compilation using existing health runners", () => {
  const ci = read("ci.yml");
  const health = read("check-health.yml");
  for (const command of ["npm run test:coverage", "node --test scripts/diagnostics/*.test.mjs", "python3 -m unittest discover", "node --test tools/help/*.test.mjs"]) {
    assert.ok(health.includes(command), `Health must retain ${command}`);
    assert.ok(!ci.includes(command), `native compilation must not wait for ${command}`);
  }
  assert.match(ci.split("  checks:\n")[1], /timeout-minutes: 90/);
  assert.match(health, /name: Upload frontend coverage/);
  assert.match(health, /files: frontend\/coverage\/lcov.info/);
});

test("all platforms run the complete frontend suite while Linux owns its coverage", () => {
  const health = read("check-health.yml");
  const coverage = step(health, "Frontend unit tests and coverage");
  const plain = step(health, "Frontend unit tests (macOS and Windows)");
  assert.ok(coverage && plain, "both execution modes must exist");
  assert.match(coverage, /^        if: runner.os == 'Linux'$/m);
  assert.match(plain, /^        if: runner.os != 'Linux'$/m);
  assert.match(coverage, /^        run: npm run test:coverage$/m);
  assert.match(plain, /^        run: npm test$/m);
  for (const selected of [coverage, plain]) {
    assert.match(selected, /^        working-directory: frontend$/m);
    assert.doesNotMatch(selected, /continue-on-error|timeout|--exclude|--testNamePattern/);
  }
  const { scripts } = JSON.parse(readFileSync(new URL("../../frontend/package.json", import.meta.url), "utf8"));
  assert.equal(scripts.test, "vitest run");
  assert.equal(scripts["test:coverage"], `${scripts.test} --coverage`);
  assert.match(step(health, "Seal browser unit line map from this run"), /^        if: runner.os == 'Linux'$/m);
});

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
  assert.match(health, /matrix:\n        os: \[ubuntu-latest, macos-26-intel, windows-latest\]/);
  const ci = read("ci.yml");
  assert.match(step(health, "Build retention safeguards (all platforms)"), /python3 -m unittest discover -s scripts -p test_build_retention.py -v/);
  for (const command of ["npm run lint", "cargo fmt", "cargo deny", "cargo machete", "shellcheck", "actionlint", "crate-ci/typos"]) assert.ok(!ci.includes(command), `duplicate ${command}`);
});

test("full browser coverage remains on every desktop OS with bounded shards", () => {
  const ui = read("e2e-ui.yml");
  const shardWorkflow = read("ui-shards.yml");
  const baseline = shardWorkflow.split("  e2e:\n")[1].split(/^  [a-z][\w-]*:\n/m)[0];
  assert.match(ui, /operating_systems: '\["ubuntu-24.04"\]'/);
  assert.match(ui, /operating_systems: '\["macos-latest","windows-latest"\]'/);
  assert.match(baseline, /os: \$\{\{ fromJSON\(inputs.operating_systems\) \}\}/);
  assert.match(baseline, /shard: \[1, 2, 3, 4\]/);
  assert.match(baseline, /fail-fast: false/);
  assert.match(step(shardWorkflow, "Run UI E2E"), /--workers=1 --retries=0 --shard=\$\{\{ matrix.shard \}\}\/4/);
  assert.match(baseline, /name: playwright-ui-\$\{\{ matrix.os \}\}-\$\{\{ matrix.shard \}\}-\$\{\{ github.run_attempt \}\}/);
  assert.match(baseline, /if-no-files-found: error/);
  assert.doesNotMatch(baseline, /test:coverage|codecov/);
});

test("the stable UI gate requires coverage merged from all Linux shards", () => {
  const ui = read("e2e-ui.yml");
  const coverage = ui.split("  coverage:\n")[1]?.split("  packs:\n")[0];
  assert.ok(coverage);
  assert.match(coverage, /needs: \[linux, health\]/);
  assert.match(coverage, /actions\/download-artifact@/);
  assert.match(coverage, /collect-ui-coverage.mjs/);
  assert.match(coverage, /merge-e2e-coverage.mjs/);
  assert.match(ui, /needs: \[linux, desktop, coverage, packs, portable-evidence, portable, chromium-extended, firefox-extended\]/);
  assert.match(ui, /COVERAGE: \$\{\{ needs.coverage.result \}\}/);
  assert.match(ui, /test "\$COVERAGE" = success/);
});

function runGate(file, name, env) {
  const gate = step(read(file), name);
  assert.ok(gate, `Missing gate: ${name}`);
  const script = gate.split("        run: |\n")[1].replace(/^          /gm, "");
  return spawnSync("bash", ["-e", "-c", script], { encoding: "utf8", env: { ...process.env, ...env } });
}

test("direct checks replace redundant runner-backed summary gates", () => {
  assert.doesNotMatch(read("ci.yml"), /^  verify:/m);
  assert.doesNotMatch(read("check-health.yml"), /^  check:/m);
  assert.doesNotMatch(read("e2e-backend.yml"), /^  backend-e2e:/m);
  assert.doesNotMatch(read("hidden-contacts-native.yml"), /^  multicast-capability:/m);
  assert.ok(step(read("hidden-contacts-native.yml"), "Diagnose multicast on the actual native test runner"));
});

test("the final UI gate rejects failed, cancelled or missing required jobs", () => {
  const env = { LINUX: "success", DESKTOP: "success", COVERAGE: "success", REPORTS: "success", PACKS: "success", CHROMIUM: "success", PORTABLE: "success", EXTENDED_CHROMIUM: "skipped", FIREFOX: "success", EXTENDED: "false" };
  assert.equal(runGate("e2e-ui.yml", "Require complete selected UI matrices", env).status, 0);
  assert.equal(runGate("e2e-ui.yml", "Require complete selected UI matrices", { ...env, REPORTS: "failure" }).status, 0, "HTML presentation is independent of test acceptance");
  for (const job of ["LINUX", "DESKTOP", "COVERAGE", "PACKS", "CHROMIUM", "PORTABLE"]) {
    for (const result of ["failure", "cancelled", "skipped", ""]) assert.notEqual(runGate("e2e-ui.yml", "Require complete selected UI matrices", { ...env, [job]: result }).status, 0, `${job}=${result}`);
  }
  assert.notEqual(runGate("e2e-ui.yml", "Require complete selected UI matrices", { ...env, EXTENDED: "true" }).status, 0);
  assert.equal(runGate("e2e-ui.yml", "Require complete selected UI matrices", { ...env, EXTENDED: "true", EXTENDED_CHROMIUM: "success", FIREFOX: "success" }).status, 0);
});

test("the protected Full Linux UI regression name remains a fail-closed check", () => {
  const coverage = read("e2e-ui.yml").split("  coverage:\n")[1]?.split("  reports:\n")[0];
  assert.match(coverage, /name: Full Linux UI regression/);
  assert.match(coverage, /if: always\(\)/);
  for (const result of ["success", "failure", "cancelled", "skipped"]) {
    assert.equal(runGate("e2e-ui.yml", "Require all browser shards", { BASELINE: result, UNIT_COVERAGE: "success" }).status === 0, result === "success");
  }
});


test("UI coverage consumers share Health's same-run Linux unit coverage", () => {
  const ui = read("e2e-ui.yml"), health = read("check-health.yml");
  assert.equal((ui.match(/run: npm run test:coverage/g) || []).length, 0);
  assert.match(ui, /health:\n    uses: \.\/\.github\/workflows\/check-health\.yml/);
  assert.match(health, /unit-coverage-artifact.mjs seal/);
  assert.match(health, /name: ui-unit-coverage-\$\{\{ github.sha \}\}-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}/);
  for (const job of ["coverage", "packs"]) {
    const consumer = ui.split(`  ${job}:\n`)[1].split(/^  [a-z][\w-]*:\n/m)[0];
    assert.match(consumer, /needs:.*health/);
    assert.match(consumer, /unit-coverage-artifact.mjs restore/);
    assert.match(consumer, /actions\/download-artifact@/);
    assert.doesNotMatch(consumer, /run-id:|github-token:/);
  }
});

test("full Chromium execution owns portable evidence while unique engines remain", () => {
  const ui = read("e2e-ui.yml");
  const portable = ui.split("  portable:\n")[1].split("  chromium-extended:\n")[0];
  assert.doesNotMatch(portable, /browser: chromium/);
  assert.match(portable, /os: macos-latest, browser: webkit/);
  assert.match(ui, /name: Chromium portable evidence/);
  assert.match(read("ui-shards.yml"), /chromium-evidence.mjs extract/);
  assert.match(ui, /chromium-evidence.mjs collect/);
  assert.match(step(read("ui-shards.yml"), "Run UI E2E"), /--reporter=list,blob,json/);
  const reports = ui.split("  reports:\n")[1].split("  packs:\n")[0];
  assert.doesNotMatch(reports, /matrix:/);
  assert.match(reports, /for os in ubuntu-24.04 macos-latest windows-latest/);
  assert.match(ui, /chromium-extended:/);
  assert.match(ui, /firefox-extended:/);
});

test("CI runs release workflow diagnostics only through the shared glob", () => {
  const ci = read("check-health.yml");
  assert.match(ci, /node --test scripts\/diagnostics\/\*.test.mjs/);
  assert.doesNotMatch(ci, /node --test scripts\/diagnostics\/release-workflow.test.mjs/);
});

test("Windows builds once then requires four complete serial archive partitions", () => {
  assert.match(read("ci.yml"), /os: \[ubuntu-latest, macos-latest\]/);
  assert.match(read("ci.yml"), /windows:\n    uses: \.\/\.github\/workflows\/windows-workspace\.yml/);
  const windows = read("windows-workspace.yml");
  assert.match(windows, /needs: build/);
  assert.match(windows, /shard: \[1, 2, 3, 4\]/);
  assert.match(windows, /ref: \$\{\{ github.sha \}\}/);
  assert.match(windows, /toolchain: \$\{\{ needs.build.outputs.rust \}\}/);
  assert.match(windows, /RUST_TEST_THREADS: '1'/);
  assert.match(windows, /cargo-nextest@0\.9\.148,cargo-llvm-cov@0\.8\.7/);
  assert.match(windows, /if-no-files-found: error/);
  assert.doesNotMatch(windows.split("  tests:\n")[1], /Swatinem\/rust-cache|cargo build|npm ci/);
});
