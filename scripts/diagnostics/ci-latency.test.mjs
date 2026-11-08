import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { versions } from "../ci/windows-coverage.mjs";
import { nextestProfileSection, replaceNextestProfileText } from "./test-fixtures/nextest.mjs";

// Also runs in cheap PR validation. Keep this owner limited to configuration
// assertions and test-inventory discovery; no suites, compilation or installs.

const read = name => readFileSync(new URL(`../../.github/workflows/${name}`, import.meta.url), "utf8");
const step = (source, name) => source.split(/^      - /m).find(value => value.startsWith(`name: ${name}\n`));

const workflowJob = (source, id) => source.split(`  ${id}:\n`)[1]?.split(/^  [a-z][\w-]*:\n/m)[0];

test("documentation scope retains every required matrix identity and guards expensive steps", () => {
  const required = {
    "ci.yml": ["checks"],
    "windows-workspace.yml": ["build", "tests"],
    "check-health.yml": ["platform", "source"],
    "e2e-backend.yml": ["backend-platform"],
    "hidden-contacts-native.yml": ["native"],
    "sanitizers.yml": ["asan"],
    "e2e-ui.yml": ["coverage", "portable-evidence", "portable", "ui-gate"],
  };
  for (const [file, ids] of Object.entries(required)) {
    const workflow = read(file);
    assert.doesNotMatch(workflow, /paths-ignore:/u, `${file}: required workflows remain discoverable`);
    for (const id of ids) {
      const block = workflowJob(workflow, id);
      assert.ok(block, `${file}/${id}`);
      assert.match(block, /if: \$\{\{ !cancelled\(\)/u, `${file}/${id}: reject failed selection rather than disappear`);
      const steps = block.split(/^      - /m).slice(1);
      assert.match(steps[0], /uses: actions\/checkout@/u);
      assert.match(steps[0], /fetch-depth: 2/u);
      assert.match(steps[1], /id: scope/u);
      assert.match(steps[1], /uses: \.\/\.github\/actions\/ci-scope/u);
      assert.match(steps[1], /upstream_result:/u);
      assert.doesNotMatch(steps[1], /continue-on-error/u);
      for (const work of steps.slice(2)) {
        const commonSource = file === "check-health.yml" && id === "source" && [
          "Install Rust", "Rust formatting", "Install source scanners",
          "Install pinned prebuilt workflow scanner", "Source and supply-chain policy",
        ].some(name => work.startsWith(`name: ${name}\n`));
        if (commonSource) {
          assert.doesNotMatch(work, /^        if:/mu, "portable source/security checks remain applicable to documentation");
          assert.doesNotMatch(work, /cargo (?:build|test|clippy)|npm run test|playwright/u);
          continue;
        }
        assert.match(work, /if: .*steps\.scope\.outputs\.full == '(?:true|false)'/u,
          `${file}/${id}: work after scope verification must explicitly select full or docs`);
      }
      if (/matrix\.os/u.test(block)) {
        assert.match(block, /runs-on: \$\{\{ .* == 'true' && matrix\.os \|\| 'ubuntu-latest' \}\}/u,
          `${file}/${id}: docs retain names without admitting native runners`);
      }
    }
  }
});

test("docs eligibility is recomputed locally and never fabricates provider or test evidence", () => {
  const selector = read("ci-scope.yml");
  assert.match(selector, /full: auto/u);
  const action = readFileSync(new URL("../../.github/actions/ci-scope/action.yml", import.meta.url), "utf8");
  assert.match(action, /scripts\/ci\/docs-scope\.mjs --expected-full "\$EXPECTED_FULL" --upstream-result "\$UPSTREAM_RESULT"/u);
  assert.match(action, /working-directory: \$\{\{ github.workspace \}\}/u);
  assert.doesNotMatch(action, /continue-on-error|checks: write|statuses: write|npm ci|rust-cache/u);
  const empty = step(read("ci.yml"), "Report documentation-only coverage through Codecov");
  assert.match(empty, /steps\.scope\.outputs\.full == 'false' && matrix\.os == 'ubuntu-latest'/u);
  assert.match(empty, /run_command: empty-upload/u);
  assert.match(empty, /version: v11\.3\.2/u);
  assert.match(empty, /fail_ci_if_error: true/u);
  assert.doesNotMatch(empty, /force:|skip_validation:|handle_no_reports_found:|dry_run:|override_commit:/u);
  const source = workflowJob(read("check-health.yml"), "source");
  for (const command of ["node scripts/validation/standards.mjs", "node scripts/check-repository-hygiene.mjs", "typos"]) {
    assert.ok(source.includes(command), `documentation validation retains ${command}`);
  }
  assert.match(read("codeql.yml"), /language: \[rust, javascript-typescript, actions\]/u);
  assert.doesNotMatch(read("codeql.yml"), /docs-scope|ci-scope|paths-ignore/u);
});

test("Linux coverage uploads one final accumulated report after ignored-case validation", () => {
  const ci = read("ci.yml");
  assert.match(step(ci, "Test + coverage (Linux)"), /workspace-coverage\.sh linux --defer-report/u);
  const final = step(ci, "Large-folder enqueue regression (Linux)");
  const validation = final.indexOf("node scripts/evals/backend-runner.mjs --validate-large-folder");
  assert.ok(validation >= 0 && validation < final.indexOf("cargo llvm-cov report"));
  assert.equal((ci.match(/cargo llvm-cov report --lcov --output-path src-tauri\/lcov\.info/gu) || []).length, 1);
  assert.match(step(ci, "Upload coverage to Codecov"), /^          files: src-tauri\/lcov\.info$/mu);
  assert.doesNotMatch(ci, /e2e-lcov\.info/u);
});

test("Codecov action and downloaded CLI versions are pinned independently", () => {
  let uploads = 0;
  for (const file of ["ci.yml", "check-health.yml", "windows-workspace.yml", "e2e-ui.yml"]) {
    for (const block of read(file).split(/^      - /m).filter(value => /uses: codecov\/codecov-action@/u.test(value))) {
      assert.match(block, /uses: codecov\/codecov-action@303a32d7a59b442fa8d48b6a1cc6825c09c847a5/u);
      assert.match(block, /^          version: v11\.3\.2$/mu, file);
      assert.match(block, /^          fail_ci_if_error: true$/mu, file);
      uploads++;
    }
  }
  assert.equal(uploads, 5);
});

test("browser and native dependency installation use the same Linux-only preparation", async () => {
  const ui = await readFile(
    new URL("../../.github/workflows/e2e-ui.yml", import.meta.url),
    "utf8",
  );
  const native = await readFile(
    new URL(
      "../../.github/workflows/hidden-contacts-native.yml",
      import.meta.url,
    ),
    "utf8",
  );
  const shards = await readFile(
    new URL("../../.github/workflows/ui-shards.yml", import.meta.url),
    "utf8",
  );
  let installers = 0;
  for (const [name, workflow] of [
    ["UI caller", ui],
    ["reusable shards", shards],
  ]) {
    for (const section of workflow.split(/^  [a-z][\w-]*:\n/m).slice(1)) {
      const install = section.indexOf("playwright install --with-deps");
      if (install === -1) continue;
      const fixedRunner = section.match(/^    runs-on: (.+)$/mu)?.[1].replace(/^['"]|['"]$/gu, "");
      if (/^(?:macos|windows)-/u.test(fixedRunner ?? "")) {
        assert.doesNotMatch(section, /name: Prepare Ubuntu archive source/u,
          `${name}: a fixed non-Linux browser runner must not prepare APT`);
        continue;
      }
      installers++;
      const preparations = section.split(
        /^      - name: Prepare Ubuntu archive source\n/m,
      );
      assert.equal(
        preparations.length,
        2,
        `${name}: every browser installer needs exactly one guarded preparation`,
      );
      const step = preparations[1].split(/^      - /m)[0];
      assert.match(step, /if: runner\.os == 'Linux'/);
      assert.match(
        step,
        /sudo --preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,RUNNER_OS/,
      );
      assert.match(step, /\.\.\/scripts\/ci\/ubuntu-apt-source\.mjs/);
      const prepare = section.indexOf("name: Prepare Ubuntu archive source");
      const setupNode = section.indexOf("uses: actions/setup-node@");
      assert.ok(
        setupNode >= 0 && prepare > setupNode,
        `${name}: establish Node before preparing APT`,
      );
      assert.ok(prepare < install, `${name}: prepare APT before the installer`);
    }
  }
  assert.ok(installers > 0, "browser dependency installers must be discovered");
  const nativePrepare = native.indexOf("name: Prepare Ubuntu archive source");
  const nativeDependencies = native.indexOf("name: Linux native webview dependencies");
  assert.ok(nativePrepare >= 0 && nativeDependencies >= 0 && nativePrepare < nativeDependencies);
  assert.match(
    native,
    /sudo --preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,RUNNER_OS.*scripts\/ci\/ubuntu-apt-source\.mjs/,
  );
  for (const name of ["ci.yml", "mutants.yml"]) {
    const workflow = await readFile(
      new URL(`../../.github/workflows/${name}`, import.meta.url),
      "utf8",
    );
    const prepare = workflow.indexOf("name: Prepare Ubuntu archive source");
    const setupNode = workflow.indexOf("uses: actions/setup-node@");
    const dependencies = workflow.indexOf("name: Install Tauri system dependencies");
    assert.ok(
      setupNode >= 0 && prepare > setupNode,
      `${name} must establish the pinned Node before preparing APT`,
    );
    assert.ok(
      dependencies >= 0 && prepare < dependencies,
      `${name} must prepare APT before Linux dependency installation`,
    );
    assert.match(
      workflow,
      /sudo --preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,RUNNER_OS.*scripts\/ci\/ubuntu-apt-source\.mjs/,
    );
  }
});

test("only auxiliary diagnostic transport is optional; acceptance artifacts stay mandatory", () => {
  const optional = {
    "check-health.yml": ["health-diagnostics-", "frontend-coverage-"],
    "ci.yml": ["runner-storage-", "native-crash-linux-", "test-timing-"],
    "sanitizers.yml": ["runner-storage-"],
    "mutants.yml": ["runner-storage-"],
    "hidden-contacts-native.yml": ["runner-storage-"],
    "release.yml": ["runner-storage-"],
    "e2e-ui.yml": ["merged-ui-", "webkit-diagnostics-"],
    "ui-shards.yml": ["ui-failure-attachments-"],
    "fuzz.yml": ["fuzz-crashes"],
  };
  const seen = [];
  for (const file of readdirSync(new URL("../../.github/workflows/", import.meta.url)).filter(file => /\.ya?ml$/u.test(file))) {
    for (const block of read(file).split(/^      - /m).filter(block => /uses: actions\/upload-artifact@/u.test(block))) {
      const artifact = block.match(/^          name: (.+)$/mu)?.[1];
      assert.ok(artifact, `${file}: upload must declare its artifact role`);
      const prefix = optional[file]?.find(prefix => artifact.startsWith(prefix));
      if (prefix) assert.match(block, /^        continue-on-error: true$/mu, `${file}: ${artifact}`);
      else assert.doesNotMatch(block, /^        continue-on-error:/mu, `${file}: ${artifact} is mandatory`);
      if (prefix) seen.push(`${file}:${prefix}`);
    }
  }
  // A declared diagnostic role may appear in both a baseline and its workers.
  assert.deepEqual([...new Set(seen)].sort(), Object.entries(optional).flatMap(([file, prefixes]) => prefixes.map(prefix => `${file}:${prefix}`)).sort());
  const diagnostics = step(read("check-health.yml"), "Diagnostic and release gate regressions (all platforms)");
  assert.doesNotMatch(diagnostics, /continue-on-error/u);
  assert.match(diagnostics, /--result tmp\/health-diagnostics\.json/u);
  assert.doesNotMatch(step(read("ci.yml"), "Collect Linux native crash evidence and retire its storage"), /continue-on-error/u);
});

test("real compiler contracts do not compete for the native job's Cargo cache", () => {
  for (const [workflow, name, expected] of [
    ["ci.yml", "Verify coverage accumulation with the actual tool", ["backend-coverage", "native-host-coverage", "rust-profile"]],
    ["windows-workspace.yml", "Validate archive and coverage contracts", ["windows-native-deps", "backend-coverage", "rust-profile", "windows-coverage-cli"]],
  ]) {
    const caller = step(read(workflow), name);
    assert.ok(caller, `${workflow}: compiler contract step is missing`);
    assert.match(caller, /run: node --test --test-concurrency=1 /, `${workflow}: real compiler contracts share Cargo's package-cache lock`);
    for (const fixture of expected) assert.ok(caller.includes(`scripts/ci/${fixture}.test.mjs`), `${workflow}: missing ${fixture}`);
    assert.doesNotMatch(caller, /continue-on-error|\|\| true|&\s*$/m);
  }
});

test("platform-exclusive probes stay in their native producer rather than universal local selection", () => {
  const contracts = JSON.parse(readFileSync(new URL("../validation/validation.json", import.meta.url), "utf8")).contracts;
  const ci = contracts.find(contract => contract.id === "ci-verification");
  assert.ok(ci, "the CI decision owner must exist");
  assert.ok(!ci.node.includes("scripts/ci/windows-native-deps.test.mjs"));
  assert.match(step(read("windows-workspace.yml"), "Validate archive and coverage contracts"), /scripts\/ci\/windows-native-deps\.test\.mjs/u);
});

test("heavy hosted Linux builds reclaim only unused image SDKs before cache expansion", () => {
  const sources = [
    ["ci.yml", read("ci.yml")],
    ["hidden-contacts-native.yml", read("hidden-contacts-native.yml")],
    ...["baseline", "mutants"].map(id => [`mutants.yml/${id}`, workflowJob(read("mutants.yml"), id)]),
    ["sanitizers.yml", read("sanitizers.yml").split("  cohort:\n")[1].split("  shards:\n")[0]],
    ["release.yml", read("release.yml").split("  build-release:\n")[1].split("  verify-release:\n")[0]],
  ];
  for (const [file, source] of sources) {
    const preparation = step(source, "Prepare hosted Linux build storage");
    assert.ok(preparation, `${file}: missing disk preparation`);
    const fixedRunner = source.match(/^    runs-on: (.+)$/mu)?.[1];
    if (!/^ubuntu-/u.test(fixedRunner ?? "")) assert.match(preparation, /runner.os == 'Linux'/);
    assert.match(preparation, /sudo --preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,RUNNER_OS/);
    assert.match(preparation, /scripts\/ci\/runner-storage\.mjs --prepare/);
    const index = source.indexOf("name: Prepare hosted Linux build storage");
    const cache = source.search(/uses: (?:Swatinem\/rust-cache|actions\/cache)@/);
    assert.ok(index >= 0 && cache > index, `${file}: restore must follow measured image reclamation`);
    assert.doesNotMatch(preparation, /cargo clean|rm -rf|hostedtoolcache|target\//);
    assert.match(preparation, /id: runner_storage/);
    assert.match(preparation, /tee "\$RUNNER_TEMP\/runner-storage\.json"/);
    const measurements = step(source, "Preserve runner storage measurements");
    assert.ok(measurements, `${file}: measurements must survive a later full-disk failure`);
    assert.match(measurements, /steps.runner_storage.outcome == 'success'/);
    assert.match(measurements, /name: runner-storage-/);
    assert.ok(source.indexOf("name: Preserve runner storage measurements") < cache);

  }
  for (const id of ["baseline", "mutants"]) {
    assert.match(workflowJob(read("mutants.yml"), id), /^    if: .*needs\.plan\.outputs\.selected != '0'/mu,
      `mutants.yml/${id}: the whole heavy job must reject an empty plan before setup`);
  }
  assert.doesNotMatch(read("ui-shards.yml"), /runner-storage\.mjs/, "browser-only shards do not need native build reclamation");
});

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
  assert.ok(toolchain >= 0 && toolchain < platform.indexOf("node --test --test-reporter=spec"));
});

test("portable validation runs beside native compilation using existing health runners", () => {
  const ci = read("ci.yml");
  const health = read("check-health.yml");
  for (const command of ["npm run test:coverage", "node --test --test-reporter=spec", "python3 -m unittest discover", "node --test tools/help/*.test.mjs"]) {
    assert.ok(health.includes(command), `Health must retain ${command}`);
    assert.ok(!ci.includes(command), `native compilation must not wait for ${command}`);
  }
  assert.match(ci.split("  checks:\n")[1], /timeout-minutes: 90/);
  assert.match(health, /name: Upload frontend coverage/);
  assert.match(health, /files: frontend\/coverage\/lcov.info/);
});

test("one complete Linux frontend unit cohort owns coverage without duplicate OS execution", () => {
  const health = read("check-health.yml");
  const coverage = step(health, "Frontend unit tests and coverage");
  assert.ok(coverage, "the complete unit coverage cohort must exist");
  assert.equal(health.split(/^      - /m).filter(value => value.startsWith("name: Frontend unit tests")).length, 1);
  assert.match(coverage, /^        if: \$\{\{ steps\.scope\.outputs\.full == 'true' && \(runner.os == 'Linux'\) \}\}$/m);
  assert.match(coverage, /^        run: npm run test:coverage$/m);
  assert.match(coverage, /^        working-directory: frontend$/m);
  assert.doesNotMatch(coverage, /continue-on-error|timeout|--exclude|--testNamePattern/);
  const { scripts } = JSON.parse(readFileSync(new URL("../../frontend/package.json", import.meta.url), "utf8"));
  assert.equal(scripts.test, "vitest run");
  assert.equal(scripts["test:coverage"], `${scripts.test} --coverage`);
  assert.match(step(health, "Seal browser unit line map from this run"), /^        if: \$\{\{ steps\.scope\.outputs\.full == 'true' && \(runner.os == 'Linux'\) \}\}$/m);
});

test("CI caches the workspace output and retains native lint and build coverage", () => {
  const ci = read("ci.yml");
  assert.match(ci, /workspaces: ['"]?\. -> target/);
  assert.match(step(ci, "Clippy"), /if: \$\{\{ steps\.scope\.outputs\.full == 'true' \}\}/u);
  assert.match(step(ci, "Build workspace"), /cargo build --workspace --locked/);
  assert.match(step(ci, "Build workspace"), /if: \$\{\{ steps\.scope\.outputs\.full == 'true' \}\}/u);
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
  assert.match(ui, /- os: macos-26-intel/);
  assert.match(ui, /- os: windows-latest/);
  assert.match(baseline, /os: \$\{\{ fromJSON\(inputs.operating_systems\) \}\}/);
  assert.match(baseline, /shard: \$\{\{ fromJSON\(inputs.shards\) \}\}/);
  assert.match(baseline, /fail-fast: false/);
  assert.match(step(shardWorkflow, "Run UI E2E"), /--workers=1 --retries=0 --shard=\$\{\{ matrix.shard \}\}\/\$\{\{ inputs.shard_total \}\}/);
  assert.match(baseline, /name: playwright-ui-\$\{\{ matrix.os \}\}-\$\{\{ matrix.shard \}\}-\$\{\{ github.run_attempt \}\}/);
  assert.match(baseline, /if-no-files-found: error/);
  assert.doesNotMatch(baseline, /test:coverage|codecov/);
});

test("the stable UI gate requires coverage merged from all Linux shards", () => {
  const ui = read("e2e-ui.yml");
  const coverage = ui.split("  coverage:\n")[1]?.split("  packs:\n")[0];
  assert.ok(coverage);
  assert.match(coverage, /needs: \[linux, health, scope\]/);
  assert.match(coverage, /actions\/download-artifact@/);
  assert.match(coverage, /collect-ui-coverage.mjs/);
  assert.match(coverage, /merge-e2e-coverage.mjs/);
  assert.match(ui, /needs: \[linux, desktop, coverage, packs, portable-evidence, portable, chromium-extended, firefox-extended, scope\]/);
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
  assert.match(coverage, /if: \$\{\{ !cancelled\(\) \}\}/);
  for (const result of ["success", "failure", "cancelled", "skipped"]) {
    assert.equal(runGate("e2e-ui.yml", "Require all browser shards", { BASELINE: result, UNIT_COVERAGE: "success" }).status === 0, result === "success");
  }
});


test("UI coverage consumers share Health's same-run Linux unit coverage", () => {
  const ui = read("e2e-ui.yml"), health = read("check-health.yml");
  assert.equal((ui.match(/run: npm run test:coverage/g) || []).length, 0);
  assert.match(ui, /uses: \.\/\.github\/workflows\/check-health\.yml/);
  assert.match(health, /unit-coverage-artifact.mjs seal/);
  assert.match(health, /name: ui-unit-coverage-\$\{\{ github.sha \}\}-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}/);
  for (const job of ["coverage"]) {
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
  assert.match(portable, /name: Portable UI \(macos-26-intel, webkit\)/);
  assert.match(portable, /needs: \[portable-shards, scope\]/);
  assert.match(ui, /name: Chromium portable evidence/);
  assert.match(read("ui-shards.yml"), /chromium-evidence.mjs extract/);
  assert.match(ui, /chromium-evidence.mjs collect/);
  assert.match(step(read("ui-shards.yml"), "Run UI E2E"), /--reporter=list,blob,json/);
  const reports = ui.split("  reports:\n")[1].split("  packs:\n")[0];
  assert.doesNotMatch(reports, /matrix:/);
  assert.match(reports, /if: \$\{\{ !cancelled\(\) && needs.scope.outputs.full == 'true' \}\}/u,
    "failed full-scope producers must still permit diagnostic report rendering");
  assert.match(reports, /chromium-evidence.mjs render-artifacts/);
  assert.match(ui, /chromium-extended:/);
  assert.match(ui, /firefox-extended:/);
});

test("CI runs release workflow diagnostics only through the shared glob", () => {
  const ci = read("check-health.yml");
  assert.match(ci, /scripts\/diagnostics\/\*.test.mjs/);
  assert.doesNotMatch(ci, /node --test scripts\/diagnostics\/release-workflow.test.mjs/);
});

test("Windows builds once then requires eight complete serial archive partitions", () => {
  assert.match(read("ci.yml"), /os: \[ubuntu-latest, macos-latest\]/);
  assert.match(read("ci.yml"), /uses: \.\/\.github\/workflows\/windows-workspace\.yml/);
  const windows = read("windows-workspace.yml");
  assert.match(windows, /needs: build/);
  assert.match(windows, /shard: \[1, 2, 3, 4, 5, 6, 7, 8\]/);
  assert.match(windows, /name: Windows workspace tests \(\$\{\{ matrix.shard \}\}\/8\)/);
  assert.match(windows, /ref: \$\{\{ github.sha \}\}/);
  assert.match(windows, /toolchain: \$\{\{ needs.build.outputs.rust \}\}/);
  assert.match(windows, /RUST_TEST_THREADS: '1'/);
  assert.ok(windows.includes(`cargo-nextest@${versions.nextest},cargo-llvm-cov@${versions.llvmCov}`));
  assert.match(windows, /if-no-files-found: error/);
  assert.doesNotMatch(windows.split("  tests:\n")[1], /Swatinem\/rust-cache|cargo build|npm ci/);
});

test("optional pack job retains unique dist checks without repeating baseline browsers", () => {
  const ui = read("e2e-ui.yml");
  const packs = ui.split("  packs:\n")[1].split("  portable-shards:\n")[0];
  assert.match(packs, /name: Optional pack UI and asset check/);
  assert.match(packs, /npm run build && node .*check-variant-assets.mjs/);
  assert.match(packs, /npm run check:build-syntax/);
  assert.match(packs, /needs: \[scope\]/);
  assert.doesNotMatch(packs, /playwright install|playwright test|coverage|codecov|download-artifact/);
});


test("ASan protocol fixture has one authoritative producer before compilation", () => {
  const sanitizer = read("sanitizers.yml");
  assert.equal((sanitizer.match(/node --test scripts\/ci\/asan-cli.test.mjs/g) ?? []).length, 1);
  assert.doesNotMatch(read("ci.yml"), /node --test[^\n]*asan-cli.test.mjs/);
  assert.ok(sanitizer.indexOf("asan-cli.test.mjs") < sanitizer.indexOf("asan.mjs build"));
});


test("removed pack browser selection remains entirely in the authoritative full Chromium inventory", () => {
  const cli = fileURLToPath(new URL("../../frontend/node_modules/@playwright/test/cli.js", import.meta.url));
  function list(args) {
    const result = spawnSync(process.execPath, [cli, "test", ...args, "--list", "--reporter=json"], { cwd: new URL("../../frontend/", import.meta.url), encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    const paths = [];
    function visit(suite) {
      for (const spec of suite.specs ?? []) for (const item of spec.tests) {
        if (args.length) assert.equal(item.expectedStatus, "passed");
        paths.push(JSON.stringify([spec.file, spec.title, item.projectName]));
      }
      for (const child of suite.suites ?? []) visit(child);
    }
    visit(JSON.parse(result.stdout));
    return paths;
  }
  const baseline = list([]);
  const removed = list(["e2e/stickers.spec.ts", "e2e/packs-empty.spec.ts", "--grep", "starts without"]);
  assert.equal(removed.length, 2);
  for (const path of removed) assert.equal(baseline.filter(candidate => candidate === path).length, 1);
});

test("historical incident rigs are manual while required native acceptance remains automatic", () => {
  for (const file of ["linux-renderer-diagnostics.yml", "linux-tray-diagnostics.yml", "native-window-diagnostics.yml", "windows-network-diagnostics.yml"]) {
    const source = read(file);
    assert.match(source, /workflow_dispatch:/);
    assert.doesNotMatch(source, /^  (?:push|pull_request|schedule):/m);
  }
  const backend = read("e2e-backend.yml");
  assert.match(backend, /pull_request:/);
  assert.match(backend, /uses: \.\/\.github\/workflows\/hidden-contacts-native\.yml/);
});

test("Windows producer validates native Perl before every Cargo build boundary", () => {
  const workflow = read("windows-workspace.yml");
  const producer = workflow.split("\n  tests:")[0];
  const bootstrap = producer.indexOf("node scripts/ci/windows-native-deps.mjs");
  assert.ok(
    bootstrap >= 0,
    "vendored OpenSSL must not inherit bare Git/MSYS perl",
  );
  for (const boundary of [
    "scripts/ci/backend-coverage.test.mjs",
    "cargo clippy",
    "cargo build",
    "cargo test",
    "cargo llvm-cov show-env",
  ]) {
    assert.ok(bootstrap < producer.indexOf(boundary), boundary);
  }
  assert.doesNotMatch(
    producer,
    /OPENSSL_NO_VENDOR:\s*['"]?1|continue-on-error:\s*true/u,
  );
});

test("actual native Perl acceptance runs in the serial Windows producer after bootstrap", () => {
  const workflow = read("windows-workspace.yml");
  const producer = workflow.split("\n  tests:")[0];
  const contract = producer.split(/^      - /m).find((step) =>
    step.startsWith("name: Validate archive and coverage contracts\n"),
  );
  assert.ok(contract);
  assert.match(contract, /run: node --test --test-concurrency=1 /u);
  assert.ok(contract.includes("scripts/ci/windows-native-deps.test.mjs"));
  assert.ok(
    producer.indexOf("node scripts/ci/windows-native-deps.mjs") <
      producer.indexOf("scripts/ci/windows-native-deps.test.mjs"),
  );
  const scopeGuard = /^        if: \$\{\{ steps\.scope\.outputs\.full == 'true' \}\}\n/mu;
  assert.match(contract, scopeGuard);
  assert.doesNotMatch(contract.replace(scopeGuard, ""), /continue-on-error|\|\| true|\n\s*if:/u);
});

test("ordinary process hang policy preserves heavy-case margin and dedicated exceptions", () => {
  const config = readFileSync(
    new URL("../../.config/nextest.toml", import.meta.url),
    "utf8",
  );
  const section = nextestProfileSection(config, "default");
  assert(section, "ordinary profile must explicitly bound stuck processes");
  const policy = section.match(
    /slow-timeout = \{ period = "(\d+)s", terminate-after = (\d+), grace-period = "5s" \}/u,
  );
  assert(policy, "ordinary process watchdog must fail and terminate");
  assert.equal(Number(policy[1]), 60);
  assert.equal(Number(policy[2]), 15);
  assert.equal(Number(policy[1]) * Number(policy[2]), 900);
  assert.match(config, /heavy.*400s/u);
  assert.match(
    config,
    /period = "15s", terminate-after = 4, grace-period = "5s"/u,
  );
  assert.match(
    config,
    /period = "30s", terminate-after = 10, grace-period = "5s"/u,
  );

  assert.doesNotMatch(config, /on-timeout\s*=\s*"pass"/u);
});

test("Nextest fixture acceleration changes only its owned profile", () => {
  const policy = 'slow-timeout = { period = "60s", terminate-after = 15, grace-period = "5s" }';
  for (const newline of ["\n", "\r\n"]) {
    const config = ['[profile.default]', policy, '[[profile.default.overrides]]', 'priority = 100', '[profile.mutants]', policy, 'test-threads = 4', ''].join(newline);
    const accelerated = replaceNextestProfileText(config, "default", policy, policy.replace('"60s"', '"100ms"'));
    assert.match(nextestProfileSection(accelerated, "default"), /period = "100ms"/u);
    assert.equal(nextestProfileSection(accelerated, "mutants"), nextestProfileSection(config, "mutants"));
    assert.equal(accelerated.slice(accelerated.indexOf('[[profile.default.overrides]]')), config.slice(config.indexOf('[[profile.default.overrides]]')));
    assert.throws(() => replaceNextestProfileText(config, "absent", policy, ""), /expected one Nextest profile/u);
    assert.throws(() => replaceNextestProfileText(config, "default", "absent policy", ""), /expected one replacement/u);
  }
});
