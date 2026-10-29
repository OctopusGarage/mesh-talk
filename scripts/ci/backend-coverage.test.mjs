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
  const workflow = readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8");
  const lines = workflow.split("\n").map(line => line.trim()).filter(line => line.startsWith("cargo llvm-cov ") && line.includes("--lib large_directory_enqueue_"));
  assert.equal(lines.length, 1, "owning large-folder command must be unique");
  const folderArgs = lines[0].split(" | tee ")[0].split(/\s+/u).slice(1);
  const folderFilter = folderArgs[folderArgs.indexOf("--lib") + 1];
  assert.match(folderFilter, /^large_directory_enqueue_[a-z_]+$/u);
  writeFileSync(join(root, "Cargo.toml"), '[package]\nname="mesh-talk-core"\nversion="0.1.0"\nedition="2024"\n');
  assert.equal(LARGE_FOLDER_SCENARIO, `node::delivery_runtime_tests::${folderFilter}`);
  writeFileSync(join(root, "src/lib.rs"), `pub fn normal_truth() -> u64 { 1 }\npub fn ignored_truth() -> u64 { 2 }\n#[test] fn normal() { assert_eq!(normal_truth(), 1); }\nmod node { mod delivery_runtime_tests { #[test] #[ignore] fn ${folderFilter}() { assert_eq!(crate::ignored_truth(), 2); } } }\n`);
  writeFileSync(join(root, "tests/two_node_cli.rs"), BACKEND_SUITES.two_node_cli.map(name => `#[test] #[ignore] fn ${name}() { assert_eq!(mesh_talk_core::ignored_truth(), 2); }`).join("\n"));
  const env = { ...process.env, CARGO_TARGET_DIR: join(root, "target"), CARGO_LLVM_COV_TARGET_DIR: join(root, "coverage-target"), CARGO_TERM_COLOR: "never" };
  delete env.LLVM_PROFILE_FILE;
  const run = args => {
    const result = spawnSync("cargo", args, { cwd: root, env, encoding: "utf8", timeout: 120_000, maxBuffer: 8 * 1024 * 1024 });
    assert.equal(result.status, 0, `${args.join(" ")}\n${result.error ?? ""}\n${result.stdout}\n${result.stderr}`);
    return result.stdout + result.stderr;
  };
  run(["generate-lockfile", "--offline"]);
  assert.match(run(["llvm-cov", "--lib", "--locked", "--lcov", "--output-path", "unit.lcov"]), /1 passed; 0 failed/);
  const output = run(backendCargoArgs("two_node_cli", true));
  validateBackendOutput(output, BACKEND_SUITES.two_node_cli);
  const folderOutput = run(folderArgs);
  assert.ok(folderOutput.split(/\r?\n/u).includes(`test ${LARGE_FOLDER_SCENARIO} ... ok`), "filtered ignored scenario did not execute");
  assert.match(folderOutput, /1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out/u);
  const logPath = join(root, "large-folder.log");
  writeFileSync(logPath, folderOutput);
  const validator = spawnSync(process.execPath, [fileURLToPath(new URL("../evals/backend-runner.mjs", import.meta.url)), "--validate-large-folder", logPath], { encoding: "utf8" });
  assert.equal(validator.status, 0, validator.stderr);
  run(["llvm-cov", "report", "--lcov", "--output-path", "result.lcov"]);
  const coverage = readFileSync(join(root, "result.lcov"), "utf8");
  assert.match(coverage, /^FNDA:[1-9]\d*,[^\n]*normal_truth/m, "earlier workspace coverage must survive the ignored pass");
  assert.match(coverage, /^FNDA:[1-9]\d*,[^\n]*ignored_truth/m, "ignored backend execution must contribute coverage");
});
