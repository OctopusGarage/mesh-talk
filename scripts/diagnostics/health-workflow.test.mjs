import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("full health builds frontend before Tauri compilation on a fresh checkout", () => {
  const script = readFileSync(new URL("../check-health.sh", import.meta.url), "utf8");
  assert.ok(script.indexOf("npm run build") < script.indexOf("cargo clippy"), "Tauri generate_context requires actual frontendDist before Clippy/tests");
  assert.equal([...script.matchAll(/npm run build/g)].length, 1, "full health should not build frontend twice");
});

test("health workflow uses a supported toolchain action and installs required scan tools", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/check-health.yml", import.meta.url), "utf8");
  assert.doesNotMatch(workflow, /actions-rs\/toolchain@/);
  assert.match(workflow, /dtolnay\/rust-toolchain@stable/);
  assert.match(workflow, /tool: cargo-deny,cargo-machete,typos-cli,shellcheck/);
  assert.match(workflow, /go install github\.com\/zricethezav\/gitleaks\/v8@v8\.30\.1/);
  assert.match(workflow, /go install github\.com\/rhysd\/actionlint\/cmd\/actionlint@v1\.7\.12/);
  assert.match(readFileSync(new URL("../check-health.sh", import.meta.url), "utf8"), /for tool in cargo-deny cargo-machete typos gitleaks shellcheck actionlint/);
});

test("Windows health and diagnostic gates explicitly execute Bash", () => {
  for (const [file, name] of [["check-health.yml", "Run unified health check"], ["ci.yml", "Diagnostic and release gate regressions (all platforms)"]]) {
    const workflow = readFileSync(new URL(`../../.github/workflows/${file}`, import.meta.url), "utf8");
    const step = workflow.split(/^      - /m).find((value) => value.startsWith(`name: ${name}\n`));
    assert.ok(step, name);
    assert.match(step, /^        shell: bash$/m, "PowerShell must not treat .sh as a file association or pass test globs literally");
  }
});

test("full health runs before merging and retains independent platform results", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/check-health.yml", import.meta.url), "utf8");
  assert.match(workflow, /^  pull_request:\n    branches: \[main\]$/m);
  assert.match(workflow, /^concurrency:\n  group: health-.*\n  cancel-in-progress: true$/m);
  assert.match(workflow, /^      fail-fast: false$/m);
});
