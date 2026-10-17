import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

// Run explicitly in mutation CI, where the pinned tool is installed. These tiny,
// dependency-free crates exercise its actual CLI and report format in seconds.
const runner = fileURLToPath(new URL("./run-mutants.mjs", import.meta.url));
for (const scenario of ["caught", "missed", "baseline failure", "empty"]) {
  test(`real cargo-mutants runner: ${scenario}`, () => {
    const root = mkdtempSync(join(tmpdir(), "mesh-mutants-runner-"));
    try {
      const cwd = join(root, "src-tauri");
      mkdirSync(join(cwd, "src"), { recursive: true });
      writeFileSync(join(root, "Cargo.toml"), '[workspace]\nmembers = ["src-tauri"]\nresolver = "3"\n[profile.mutants]\ninherits = "test"\ndebug = "none"\nincremental = true\n');
      writeFileSync(join(cwd, "Cargo.toml"), '[package]\nname = "mutation-smoke"\nversion = "0.1.0"\nedition = "2024"\n');
      const assertion = scenario === "baseline failure" ? "assert!(super::eligible(1));" : "assert!(super::eligible(11));";
      const negative = scenario === "caught" ? "assert!(!super::eligible(1));" : "";
      const source = `pub fn eligible(x: u32) -> bool { x > 10 }\n#[cfg(test)] mod tests { #[test] fn accepts() { ${assertion} ${negative} } }\n`;
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
          CARGO_INCREMENTAL: "1",
          CARGO_TARGET_DIR: join(root, "target"),
          GITHUB_STEP_SUMMARY: join(root, "summary.md"),
        },
        encoding: "utf8",
        timeout: 60_000,
      });
      assert.ifError(result.error);
      const output = result.stdout + result.stderr;
      assert.equal(result.status, scenario === "baseline failure" ? 1 : 0, output);
      assert.equal(readFileSync(sourcePath, "utf8"), source, "in-place mutation must restore source");
      if (scenario === "baseline failure") {
        assert.match(output, /mutation tool exit indicates failure: 4/);
      } else {
        const summary = readFileSync(join(root, "summary.md"), "utf8");
        assert.match(summary, new RegExp(`"selected":${scenario === "empty" ? 0 : 1}`));
        if (scenario !== "empty") assert.match(summary, new RegExp(`"${scenario}":1`));
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}
