import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

// Run explicitly in mutation CI, where the pinned tool is installed. These tiny,
// dependency-free crates exercise its actual CLI and report format in seconds.
const runner = fileURLToPath(new URL("./run-mutants.mjs", import.meta.url));
test("selection preflight uses the real parser without compiling or running a failing baseline", t => {
  const root = mkdtempSync(join(tmpdir(), "mesh-mutants-selection-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const cwd = join(root, "src-tauri");
  mkdirSync(join(cwd, "src"), { recursive: true });
  writeFileSync(join(root, "Cargo.toml"), '[workspace]\nmembers = ["src-tauri"]\nresolver = "3"\n[profile.mutants]\ninherits = "test"\n');
  writeFileSync(join(cwd, "Cargo.toml"), '[package]\nname = "selection-smoke"\nversion = "0.1.0"\nedition = "2024"\n');
  writeFileSync(join(cwd, "src/lib.rs"), 'pub fn eligible(x: u32) -> bool { x > 10 }\n#[cfg(test)] mod tests { #[test] fn fails() { panic!("baseline must not run"); } }\n');
  for (const [runNumber, count] of [["1", 1], ["256", 0]]) {
    const output = join(root, `output-${runNumber}`);
    const result = spawnSync(process.execPath, [runner, "--list-only"], {
      cwd, encoding: "utf8", timeout: 30_000,
      env: { ...process.env, GITHUB_RUN_NUMBER: runNumber, MUTATION_WORKER: "0", MUTATION_WORKERS: "1", GITHUB_OUTPUT: output },
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.equal(readFileSync(output, "utf8"), `selected=${count}\n`);
  }
  const invalid = spawnSync(process.execPath, [runner, "--list-only", "--in-diff", "missing.patch"], {
    cwd, encoding: "utf8", timeout: 30_000,
    env: { ...process.env, GITHUB_RUN_NUMBER: "1", MUTATION_WORKER: "0", MUTATION_WORKERS: "1" },
  });
  assert.ifError(invalid.error);
  assert.notEqual(invalid.status, 0, "invalid selection must never certify an empty sample");
});
for (const scenario of ["caught", "missed", "baseline failure", "empty", "hanging mutant", "hanging baseline"]) {
  test(`real cargo-mutants runner: ${scenario}`, () => {
    const root = mkdtempSync(join(tmpdir(), "mesh-mutants-runner-"));
    try {
      const cwd = join(root, "src-tauri");
      mkdirSync(join(cwd, "src"), { recursive: true });
      writeFileSync(join(root, "Cargo.toml"), '[workspace]\nmembers = ["src-tauri"]\nresolver = "3"\n[profile.mutants]\ninherits = "test"\ndebug = "none"\nincremental = true\n');
      writeFileSync(join(cwd, "Cargo.toml"), '[package]\nname = "mutation-smoke"\nversion = "0.1.0"\nedition = "2024"\n');
      const assertion = scenario === "baseline failure" ? "assert!(super::eligible(1));" : "assert!(super::eligible(11));";
      const negative = scenario === "caught" ? "assert!(!super::eligible(1));" : "";
      const source = scenario.startsWith("hanging") ? `pub fn advance() -> bool { ${scenario === "hanging baseline" ? "false" : "true"} }\n#[cfg(test)] mod tests { #[test] fn completes() { while !super::advance() {} } }\n` : `pub fn eligible(x: u32) -> bool { x > 10 }\n#[cfg(test)] mod tests { #[test] fn accepts() { ${assertion} ${negative} } }\n`;
      mkdirSync(join(root, ".config"), { recursive: true });
      writeFileSync(join(root, ".config/nextest.toml"), '[profile.mutants]\nslow-timeout = { period = "1s", terminate-after = 1, grace-period = "1s" }\ntest-threads = 2\n');
      const sourcePath = join(cwd, "src/lib.rs");
      writeFileSync(sourcePath, source);
      const args = [runner];
      if (scenario === "caught") {
        const patch = join(root, "pr.diff");
        writeFileSync(patch, "diff --git a/src-tauri/src/lib.rs b/src-tauri/src/lib.rs\n--- a/src-tauri/src/lib.rs\n+++ b/src-tauri/src/lib.rs\n@@ -1 +1 @@\n-pub fn eligible(x: u32) -> bool { x > 9 }\n+pub fn eligible(x: u32) -> bool { x > 10 }\n");
        args.push("--in-diff", patch);
      }
      const result = spawnSync(process.execPath, args, {
        cwd,
        env: {
          ...process.env,
          GITHUB_RUN_NUMBER: scenario === "empty" ? "256" : "1",
          MUTATION_WORKER: "0",
          MUTATION_WORKERS: "1",
          // Match the environment exported by rust-cache before the runner.
          CARGO_INCREMENTAL: "0",
          CARGO_TARGET_DIR: join(root, "target"),
          GITHUB_STEP_SUMMARY: join(root, "summary.md"),
        },
        encoding: "utf8",
        timeout: 60_000,
      });
      assert.ifError(result.error);
      const output = result.stdout + result.stderr;
      assert.equal(result.status, scenario.includes("baseline") ? 1 : 0, output);
      assert.equal(readFileSync(sourcePath, "utf8"), source, "in-place mutation must restore source");
      if (scenario === "caught") {
        const incremental = join(root, "target/mutants/incremental");
        const crates = readdirSync(incremental, { withFileTypes: true }).filter((entry) => entry.isDirectory() && entry.name.startsWith("mutation_smoke-"));
        assert.ok(crates.some((crate) => readdirSync(join(incremental, crate.name), { withFileTypes: true }).some((session) => session.isDirectory() && session.name.startsWith("s-") && statSync(join(incremental, crate.name, session.name, "query-cache.bin"), { throwIfNoEntry: false })?.size > 0)), "the real Rust compilation must populate its incremental query cache even when the parent environment disables it");
      }
      if (scenario.includes("baseline")) {
        assert.match(output, /mutation tool exit indicates failure: 4/);
      } else {
        const summary = readFileSync(join(root, "summary.md"), "utf8");
        assert.match(summary, new RegExp(`"selected":${scenario === "empty" ? 0 : 1}`));
        if (scenario !== "empty") assert.match(summary, new RegExp(`"${scenario === "hanging mutant" ? "caught" : scenario}":1`));
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}
