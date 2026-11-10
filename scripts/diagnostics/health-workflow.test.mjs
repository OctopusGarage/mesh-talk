import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("full health builds frontend before Tauri compilation on a fresh checkout", () => {
  const script = readFileSync(new URL("../check-health.sh", import.meta.url), "utf8");
  assert.ok(script.indexOf("npm run build") < script.indexOf("cargo clippy"), "Tauri generate_context requires actual frontendDist before Clippy/tests");
  assert.equal([...script.matchAll(/npm run build/g)].length, 1, "full health should not build frontend twice");
});

test("health installs source scanners while Gitleaks owns the full-history scan", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/check-health.yml", import.meta.url), "utf8");
  assert.doesNotMatch(workflow, /actions-rs\/toolchain@/);
  assert.match(workflow, /dtolnay\/rust-toolchain@stable/);
  assert.match(workflow, /tool: cargo-deny,cargo-machete,typos-cli,shellcheck/);
  const secrets = readFileSync(new URL("../../.github/workflows/gitleaks.yml", import.meta.url), "utf8");
  assert.match(secrets, /fetch-depth: 0/);
  assert.match(secrets, /run: bash scripts\/ci-gitleaks\.sh/u);
  assert.match(secrets, /GH_TOKEN: \$\{\{ github\.token \}\}/u);
  assert.doesNotMatch(secrets, /curl|tar -xz/u);
  const scanner = readFileSync(new URL("../ci-gitleaks.sh", import.meta.url), "utf8");
  assert.match(scanner, /readonly version='8\.30\.1'/u);
  assert.match(scanner, /readonly digest='[a-f0-9]{64}'/u);
  assert.match(scanner, /"\$scanner" git \. --config \.gitleaks\.toml --redact -v/u);
  assert.match(workflow, /https:\/\/github\.com\/rhysd\/actionlint\/releases\/download\/v1\.7\.12\/actionlint_1\.7\.12_linux_amd64\.tar\.gz/);
  assert.match(workflow, /8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8/);
  assert.match(workflow, /sha256sum --check/);
  assert.doesNotMatch(workflow, /go install|setup-go|tool: .*actionlint/);
  assert.match(readFileSync(new URL("../check-health.sh", import.meta.url), "utf8"), /for tool in cargo-deny cargo-machete typos gitleaks shellcheck actionlint/);
});

test("Windows health and diagnostic gates explicitly execute Bash", () => {
  for (const [file, name] of [["check-health.yml", "Diagnostic and release gate regressions (all platforms)"]]) {
    const workflow = readFileSync(new URL(`../../.github/workflows/${file}`, import.meta.url), "utf8");
    const step = workflow.split(/^      - /m).find((value) => value.startsWith(`name: ${name}\n`));
    assert.ok(step, name);
    assert.match(step, /^        shell: bash$/m, "PowerShell must not treat .sh as a file association or pass test globs literally");
  }
  assert.match(readFileSync(new URL("../../.github/workflows/check-health.yml", import.meta.url), "utf8"), /defaults:\n      run:\n        shell: bash/);
});

test("health runs for every PR and retains independent platform results", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/check-health.yml", import.meta.url), "utf8");
  assert.match(workflow, /^  workflow_call:$/m);
  const caller = readFileSync(new URL("../../.github/workflows/e2e-ui.yml", import.meta.url), "utf8");
  assert.match(caller, /^  pull_request:$/m);
  assert.match(caller, /uses: \.\/\.github\/workflows\/check-health\.yml/);
  assert.match(workflow, /^concurrency:\n  group: health-.*\n  cancel-in-progress: \$\{\{ github.event_name == 'pull_request' \|\| github.event_name == 'merge_group' \}\}$/m);
  assert.match(workflow, /^      fail-fast: false$/m);
});

test("health reports cheap formatting and frontend static failures before expensive suites", () => {
  const script = readFileSync(new URL("../check-health.sh", import.meta.url), "utf8");
  assert.ok(script.includes("cargo fmt --all -- --check"));
  assert.ok(script.indexOf("cargo fmt --all -- --check") < script.indexOf('scripts/ai-eval-smoke.sh'));
  for (const command of ['npm run format:check', 'npm run lint', 'npm run typecheck']) {
    assert.ok(script.includes(command), `missing static command: ${command}`);
    assert.ok(script.indexOf(command) < script.indexOf('node --test scripts/test-ai-eval.mjs'), command);
    assert.ok(script.indexOf(command) < script.indexOf('npm run build'), command);
  }
});

test("affected preflight checks frontend formatting before diagnostics and compilation", () => {
  const script = readFileSync(new URL("../pr-preflight.sh", import.meta.url), "utf8");
  const command = 'node "$stage" frontend-format --profile shared --inputs frontend -- npm run format:check';
  assert.ok(script.includes(command));
  assert.ok(script.indexOf(command) < script.indexOf('node "$stage" diagnostics'));
  assert.ok(script.indexOf(command) < script.indexOf('node "$stage" rust-selectors'));
});

test("full health holds a managed checkout lease and never reuses its aggregate verdict", async t => {
  const { mkdtempSync, mkdirSync, copyFileSync, writeFileSync, rmSync } = await import("node:fs");
  const { join, resolve } = await import("node:path");
  const { tmpdir } = await import("node:os");
  const { spawnSync } = await import("node:child_process");
  const root = mkdtempSync(join(tmpdir(), "mesh health lease "));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "scripts/validation"), { recursive: true });
  mkdirSync(join(root, "frontend")); mkdirSync(join(root, "src-tauri"));
  mkdirSync(join(root, "frontend/node_modules"));
  writeFileSync(join(root, "frontend/node_modules/.package-lock.json"), "{}");
  writeFileSync(join(root, "Makefile"), "");
  copyFileSync(resolve("scripts/check-health.sh"), join(root, "scripts/check-health.sh"));
  writeFileSync(join(root, "scripts/validation/standards.mjs"), "");
  const trace = join(root, "trace.json");
  writeFileSync(join(root, "scripts/validation/stage.mjs"), "import fs from 'node:fs'; fs.writeFileSync(process.env.TRACE,JSON.stringify({args:process.argv.slice(2),fresh:process.env.MESH_VALIDATION_FRESH}));process.exit(17);\n");
  for (const fresh of ["0", "1"]) {
    const result = spawnSync("bash", [join(root, "scripts/check-health.sh"), "--fast"], { cwd: root, encoding: "utf8", env: { ...process.env, TRACE: trace, MESH_VALIDATION_FRESH: fresh, MESH_VALIDATION_HEALTH_GUARDED: "0" } });
    assert.equal(result.status, 17, result.stderr || result.stdout);
    const seen = JSON.parse(readFileSync(trace, "utf8"));
    assert.equal(seen.fresh, "1", "aggregate health must execute on every invocation");
    assert.deepEqual(seen.args.slice(0, 8), ["full-health", "--profile", "shared", "--resource", "health", "--", "env", `MESH_VALIDATION_FRESH=${fresh}`]);
    assert.ok(seen.args.includes("MESH_VALIDATION_HEALTH_GUARDED=1"));
    const python = seen.args.indexOf("python3");
    assert.match(seen.args[python + 1], /scripts\/build-retention\.py$/u);
    assert.deepEqual(seen.args.slice(python + 2, python + 5), ["run", "--", "bash"]);
    assert.equal(seen.args.at(-1), "--fast");
  }
});

test("health primes fresh frontend dependencies before capturing supervised inputs", { skip: process.platform === "win32" ? "POSIX npm shim; aggregate supervision remains tested on Windows" : false }, async t => {
  const { mkdtempSync, mkdirSync, copyFileSync, writeFileSync, rmSync } = await import("node:fs");
  const { join, resolve, delimiter } = await import("node:path");
  const { tmpdir } = await import("node:os");
  const { spawnSync } = await import("node:child_process");
  const root = mkdtempSync(join(tmpdir(), "mesh health bootstrap "));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const name of ["scripts/validation", "frontend", "src-tauri", "bin"]) mkdirSync(join(root, name), { recursive: true });
  writeFileSync(join(root, "Makefile"), "");
  copyFileSync(resolve("scripts/check-health.sh"), join(root, "scripts/check-health.sh"));
  writeFileSync(join(root, "scripts/validation/standards.mjs"), "");
  writeFileSync(join(root, "bin/npm"), "#!/bin/sh\nmkdir -p node_modules\nprintf '{}' > node_modules/.package-lock.json\n", { mode: 0o755 });
  const trace = join(root, "trace.json");
  writeFileSync(join(root, "scripts/validation/stage.mjs"), "import fs from 'node:fs';fs.writeFileSync(process.env.TRACE,JSON.stringify({primed:fs.existsSync('frontend/node_modules/.package-lock.json')}));process.exit(17);\n");
  const result = spawnSync("bash", [join(root, "scripts/check-health.sh"), "--fast"], { cwd: root, encoding: "utf8", env: { ...process.env, TRACE: trace, MESH_VALIDATION_HEALTH_GUARDED: "0", PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}` } });
  assert.equal(result.status, 17, result.stderr || result.stdout);
  assert.equal(JSON.parse(readFileSync(trace, "utf8")).primed, true);
});
