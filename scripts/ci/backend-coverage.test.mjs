// Requires the real cargo-llvm-cov/llvm-tools installed by the owning CI job.
// Health's portable diagnostics intentionally do not require coverage tooling.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { BACKEND_SUITES, LARGE_FOLDER_SCENARIO, backendCargoArgs, validateBackendOutput } from "../evals/backend-runner.mjs";

test("the real coverage CLI accumulates workspace and ignored backend execution", t => {
  const root = mkdtempSync(join(tmpdir(), "mesh-backend-coverage-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const name of ["src", "tests"]) mkdirSync(join(root, name));
  const folderCommand = workflowName => {
    const workflow = readFileSync(new URL(`../../.github/workflows/${workflowName}`, import.meta.url), "utf8");
    const lines = workflow.split("\n").map(line => line.trim());
    const commands = lines.filter(line => line.startsWith("cargo ") && line.includes("--test large_directory_enqueue "));
    assert.equal(commands.length, 1, "owning large-folder command must be unique");
    assert.ok(lines.includes("set -o pipefail"), "failed Cargo execution must survive tee");
    assert.match(commands[0], / 2> "\$RUNNER_TEMP\/backend-eval-results\/large-folder\.stderr\.log" \| tee /u, "machine evidence must exclude stderr");
    const args = commands[0].split(" 2> ")[0].split(/\s+/u).slice(1);
    assert.ok(args.includes("--locked"));
    assert.equal(args[args.indexOf("--test") + 2], LARGE_FOLDER_SCENARIO);
    assert.deepEqual(args.slice(args.indexOf("--") + 1), ["--exact", "--ignored", "--test-threads=1"]);
    return args;
  };
  const folderArgs = folderCommand("ci.yml");
  const nativeFolderArgs = folderCommand("e2e-backend.yml");
  assert.deepEqual(folderArgs.slice(0, 3), ["llvm-cov", "test", "--no-report"]);
  assert.equal(nativeFolderArgs[0], "test");
  assert.deepEqual(folderArgs.slice(3), nativeFolderArgs.slice(1));
  writeFileSync(join(root, "Cargo.toml"), '[package]\nname="mesh-talk-core"\nversion="0.1.0"\nedition="2024"\n');
  writeFileSync(join(root, "src/lib.rs"), "pub fn normal_truth() -> u64 { 1 }\npub fn ignored_truth() -> u64 { 2 }\npub fn large_folder_truth() -> u64 { 3 }\n#[test] fn normal() { assert_eq!(normal_truth(), 1); }\n");
  // A second prefix-matching case proves --exact is honored by the actual CLI.
  writeFileSync(join(root, "tests/large_directory_enqueue.rs"), `#[test] #[ignore] fn ${LARGE_FOLDER_SCENARIO}() { assert_eq!(mesh_talk_core::large_folder_truth(), 3); }\n#[test] #[ignore] fn ${LARGE_FOLDER_SCENARIO}_unrelated() { panic!("exact selection must exclude this case"); }\n`);
  writeFileSync(join(root, "tests/two_node_cli.rs"), BACKEND_SUITES.two_node_cli.map(name => `#[test] #[ignore] fn ${name}() { assert_eq!(mesh_talk_core::ignored_truth(), 2); }`).join("\n"));
  const env = { ...process.env, CARGO_TARGET_DIR: join(root, "target"), CARGO_LLVM_COV_TARGET_DIR: join(root, "coverage-target"), CARGO_TERM_COLOR: "never" };
  delete env.LLVM_PROFILE_FILE;
  const run = args => {
    const result = spawnSync("cargo", args, { cwd: root, env, encoding: "utf8", timeout: 120_000, maxBuffer: 8 * 1024 * 1024 });
    assert.equal(result.status, 0, `${args.join(" ")}\n${result.error ?? ""}\n${result.stdout}\n${result.stderr}`);
    return result.stdout;
  };
  run(["generate-lockfile", "--offline"]);
  assert.match(run(["llvm-cov", "--lib", "--locked", "--lcov", "--output-path", "unit.lcov"]), /1 passed; 0 failed/);
  const output = run(backendCargoArgs("two_node_cli", true));
  validateBackendOutput(output, BACKEND_SUITES.two_node_cli);
  const folderOutput = run(folderArgs);
  assert.ok(folderOutput.split(/\r?\n/u).includes(`test ${LARGE_FOLDER_SCENARIO} ... ok`), "filtered ignored scenario did not execute");
  assert.match(folderOutput, /1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out/u);
  validateBackendOutput(run(nativeFolderArgs), [LARGE_FOLDER_SCENARIO], { allowFiltered: true });
  const logPath = join(root, "large-folder.log");
  writeFileSync(logPath, folderOutput);
  const validator = spawnSync(process.execPath, [fileURLToPath(new URL("../evals/backend-runner.mjs", import.meta.url)), "--validate-large-folder", logPath], { encoding: "utf8" });
  assert.equal(validator.status, 0, validator.stderr);
  run(["llvm-cov", "report", "--lcov", "--output-path", "result.lcov"]);
  const coverage = readFileSync(join(root, "result.lcov"), "utf8");
  assert.match(coverage, /^FNDA:[1-9]\d*,[^\n]*normal_truth/m, "earlier workspace coverage must survive the ignored pass");
  assert.match(coverage, /^FNDA:[1-9]\d*,[^\n]*ignored_truth/m, "ignored backend execution must contribute coverage");
  assert.match(coverage, /^FNDA:[1-9]\d*,[^\n]*large_folder_truth/m, "the separate ignored integration target must contribute coverage");
});

test("large-folder workflow commands preserve stdout evidence and actual Cargo failure", { skip: process.platform === "win32" }, t => {
  const root = mkdtempSync(join(tmpdir(), "mesh-folder-streams-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "bin"));
  mkdirSync(join(root, "backend-eval-results"));
  writeFileSync(join(root, "bin/cargo"), "#!/usr/bin/env bash\nprintf 'diagnostic-prefix' >&2\nprintf '%s\\n' 'test large_directory_enqueue_uses_compact_manifest_instead_of_frame_error ... ok' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'\nexit \"$MESH_FIXTURE_CARGO_EXIT\"\n", { mode: 0o755 });
  for (const workflowName of ["ci.yml", "e2e-backend.yml"]) {
    const workflow = readFileSync(new URL(`../../.github/workflows/${workflowName}`, import.meta.url), "utf8");
    const step = workflow.split(/^      - /m).find(value => value.includes("--test large_directory_enqueue "));
    const block = step.split("        run: |\n")[1].replace(/^          /gm, "");
    for (const code of [0, 37]) {
      const result = spawnSync("bash", ["-e", "-o", "pipefail", "-c", block], {
        cwd: fileURLToPath(new URL("../../", import.meta.url)), encoding: "utf8", timeout: 15000,
        env: { ...process.env, RUNNER_TEMP: root, PATH: `${join(root, "bin")}:${process.env.PATH}`, MESH_FIXTURE_CARGO_EXIT: String(code) },
      });
      assert.equal(result.status, code, `${workflowName}: ${result.stderr}`);
      assert.match(result.stderr, /diagnostic-prefix/);
      assert.doesNotMatch(readFileSync(join(root, "backend-eval-results/large-folder.log"), "utf8"), /diagnostic-prefix/);
      if (code !== 0) assert.doesNotMatch(result.stdout, /Large-folder acceptance/);
    }
  }
});
