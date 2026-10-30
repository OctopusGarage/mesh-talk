// Requires the pinned nextest/llvm-cov tools and matching llvm-tools-preview.
// Runs outside the portable Health glob; every Git commit is inside this temp fixture.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync, copyFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

test("real pinned archive CLI preserves serial shards, coverage, Cargo runtime and clean source", { timeout: 120000 }, t => {
  const temporary = mkdtempSync(path.join(tmpdir(), "mesh-windows-archive-"));
  t.after(() => rmSync(temporary, { recursive: true, force: true }));
  const root = path.join(temporary, "source");
  for (const directory of ["src", "tests", "examples", ".cargo", ".config", "empty-hooks"]) mkdirSync(path.join(root, directory), { recursive: true });
  const write = (file, text) => writeFileSync(path.join(root, file), text);
  write(".gitignore", "/target/\n");
  write(".cargo/config.toml", "[build]\njobs = 1\n");
  write(".config/nextest.toml", "[profile.default]\ntest-threads = 1\nretries = 0\nfail-fast = false\n");
  write("Cargo.toml", '[package]\nname="archive-fixture"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[[example]]\nname="default-example"\ntest=true\n');
  write("src/lib.rs", `
/// \`\`\`
/// assert_eq!(archive_fixture::branch(true), 7);
/// \`\`\`
pub fn branch(first: bool) -> u8 { if first { 7 } else { 9 } }
#[cfg(test)] mod tests {
  #[test] fn first() { println!("PHASE {{\\\"successful\\\":true}}"); assert_eq!(crate::branch(true), 7); }
  #[test] fn second() { assert_eq!(crate::branch(false), 9); }
  #[test] #[ignore] fn ignored() { panic!("must stay ignored"); }
}
`);
  write("examples/default-example.rs", 'fn main() {}\n#[test] fn example_body() { assert_eq!(archive_fixture::branch(true), 7); }\n');
  write("src/main.rs", 'fn main() { println!("helper"); }\n');
  write("tests/runtime.rs", `
#[test] fn cargo_runtime_contract() {
 assert_eq!(std::env::current_dir().unwrap(), std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()));
 for binary in [std::env::var("CARGO_BIN_EXE_archive-fixture").unwrap(), env!("CARGO_BIN_EXE_archive-fixture").to_owned()] {
   assert!(std::process::Command::new(binary).output().unwrap().status.success());
 }
}
`);
  copyFileSync(fileURLToPath(new URL("./windows-coverage.mjs", import.meta.url)), path.join(root, "windows-coverage.mjs"));
  const baseEnv = { ...process.env, CARGO_NET_OFFLINE: "true", RUST_TEST_THREADS: "1", CARGO_TERM_COLOR: "never", CARGO_PROFILE_DEV_DEBUG: "line-tables-only", CARGO_PROFILE_TEST_DEBUG: "line-tables-only" };
  // Fixture compilation must not inherit an outer workspace's instrumentation paths.
  for (const name of Object.keys(baseEnv)) if (name.startsWith("CARGO_LLVM_COV") || name.startsWith("__CARGO_LLVM_COV") || ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_TARGET_DIR", "LLVM_PROFILE_FILE", "CARGO_BUILD_TARGET"].includes(name)) delete baseEnv[name];
  let latestStderr = "";
  const run = (binary, args, env = baseEnv, expectedStatus = 0) => {
    const result = spawnSync(binary, args, { cwd: root, encoding: "utf8", env, timeout: 60000, maxBuffer: 16 * 1024 * 1024 });
    latestStderr = result.stderr ?? "";
    assert.equal(result.status, expectedStatus, `${binary} ${args.join(" ")}\n${result.stdout}\n${result.stderr}\n${result.error ?? ""}`);
    return result.stdout.trim();
  };
  run("cargo", ["generate-lockfile", "--offline"]);
  run("cargo", ["test", "--doc", "--offline"]);
  run("git", ["init", "--quiet"]);
  run("git", ["add", "."]);
  run("git", ["-c", "user.name=CI Fixture", "-c", "user.email=fixture@example.invalid", "-c", "core.hooksPath=empty-hooks", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "test: isolated archive fixture"]);
  const env = { ...baseEnv, GITHUB_SHA: run("git", ["rev-parse", "HEAD"]), GITHUB_RUN_ID: "1", GITHUB_RUN_ATTEMPT: "1", MESH_TALK_TEST_TIMING: "1", MESH_ARCHIVE_NODE: process.execPath.replaceAll("\\", "/") };
  const artifact = path.join(temporary, "archive");
  const phase = (args, environment, expectedStatus = 0) => {
    // Script paths and argv stay in environment variables; no shell interpolation.
    const scoped = { ...environment };
    args.forEach((arg, index) => { scoped[`MESH_ARCHIVE_ARG_${index}`] = arg; });
    const script = 'eval "$(cargo llvm-cov show-env --sh)"\n"$MESH_ARCHIVE_NODE" windows-coverage.mjs ' + args.map((_, index) => `"$MESH_ARCHIVE_ARG_${index}"`).join(" ");
    run("bash", ["-euc", script], scoped, expectedStatus);
    if (expectedStatus !== 0) {
      assert.match(latestStderr, /Consumer identity differs from producer/);
      return;
    }
    if (args[0] === "consume") assert.doesNotMatch(latestStderr, /^\s*(?:Compiling|Checking) /mu, "consumer compiled instead of reusing archive");
    assert.equal(run("git", ["status", "--porcelain", "--untracked-files=normal"]), "", "instrumented listing dirtied source");
    assert.ok(!readdirSync(root).some(name => /^default.*\.profraw$/u.test(name)));
  };
  phase(["produce", artifact], env);
  renameSync(path.join(root, "target"), path.join(temporary, "producer-target"));
  phase(["consume", "1", artifact, path.join(temporary, "rejected-profile")], { ...env, CARGO_PROFILE_TEST_DEBUG: "full" }, 1);
  const freshCargoHome = path.join(temporary, "fresh-cargo-home");
  mkdirSync(freshCargoHome);
  const receipts = [], logs = [];
  for (const shard of [1, 2]) {
    const output = path.join(temporary, `results-${shard}`);
    phase(["consume", String(shard), artifact, output], { ...env, CARGO_HOME: freshCargoHome });
    receipts.push(JSON.parse(readFileSync(path.join(output, "receipt.json"), "utf8")));
    logs.push(readFileSync(path.join(output, "execution.log"), "utf8"));
    renameSync(path.join(root, "target"), path.join(temporary, `consumer-${shard}-target`));
  }
  assert.equal(receipts.reduce((total, receipt) => total + receipt.executedTests, 0), 4);
  assert.ok(receipts.every(receipt => receipt.totalTests === 5 && receipt.coverage.coveredLines > 0));
  assert.equal(receipts[0].archiveSha256, receipts[1].archiveSha256);
  assert.ok(logs.some(log => log.includes('PHASE {"successful":true}')), "successful timing payload was lost");
});
