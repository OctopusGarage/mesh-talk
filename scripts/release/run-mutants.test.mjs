import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
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
      const lock = spawnSync("cargo", ["generate-lockfile", "--offline"], { cwd: root, encoding: "utf8" });
      assert.equal(lock.status, 0, lock.stderr);
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

for (const sharedCase of ["caught", "missed", "baseline failure"]) {
test(`real shared baseline: ${sharedCase}, rerun identity and raw report`, { timeout: 120000, skip: process.platform !== "linux" && "kernel child ownership requires Linux" }, async () => {
  const { execFileSync } = await import("node:child_process");
  const { produceMutationPlan, produceMutationBaseline, runMutationWorker } = await import("./mutation-baseline.mjs");
  const { evidenceDigest } = await import("./mutation-evidence.mjs");
  const root = mkdtempSync(join(tmpdir(), "mesh-shared-mutation-"));
  const evidence = mkdtempSync(join(tmpdir(), "mesh-shared-mutation-evidence-"));
  const previous = process.cwd(), saved = { ...process.env };
  try {
    const cwd = join(root, "src-tauri");
    mkdirSync(join(cwd, "src"), { recursive: true }); mkdirSync(join(root, ".config")); mkdirSync(join(root, ".cargo"));
    writeFileSync(join(root, ".gitignore"), "/target/\n/evidence/\n");
    writeFileSync(join(root, ".cargo/config.toml"), "[build]\njobs=2\n");
    writeFileSync(join(root, "Cargo.toml"), '[workspace]\nmembers=["src-tauri"]\nresolver="3"\n[profile.mutants]\ninherits="test"\ndebug="none"\nincremental=true\n');
    writeFileSync(join(cwd, "Cargo.toml"), '[package]\nname="mutation-shared-fixture"\nversion="0.1.0"\nedition="2024"\n');
    writeFileSync(join(cwd, "src/lib.rs"), `pub fn eligible(x:u32)->bool { x>10 }\n#[test] fn accepts() { ${sharedCase === "baseline failure" ? "assert!(eligible(1));" : "assert!(eligible(11));"} ${sharedCase === "caught" ? "assert!(!eligible(1));" : ""} }\n`);
    writeFileSync(join(root, ".config/nextest.toml"), '[profile.mutants]\nslow-timeout={period="1s",terminate-after=1,grace-period="1s"}\ntest-threads=4\n');
    execFileSync("cargo", ["generate-lockfile", "--offline"], { cwd: root });
    for (const args of [["init", "-q"], ["config", "user.name", "fixture"], ["config", "user.email", "fixture@example.invalid"], ["config", "core.hooksPath", "empty-hooks"], ["add", "."], ["commit", "-qm", "fixture"]]) execFileSync("git", args, { cwd: root });
    Object.assign(process.env, { GITHUB_SHA: execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim(), GITHUB_RUN_ID: "123", GITHUB_RUN_NUMBER: "1", GITHUB_RUN_ATTEMPT: "1", GITHUB_WORKFLOW: "fixture", CARGO_TARGET_DIR: join(root, "target") });
    delete process.env.GITHUB_OUTPUT;
    process.chdir(cwd);
    const planDir = join(evidence, "plan"), baselineDir = join(evidence, "baseline");
    const plan = await produceMutationPlan(planDir);
    Object.assign(process.env, { MUTATION_PLAN_DIGEST: evidenceDigest(plan), MUTATION_PLAN_ATTEMPT: "1" });
    if (sharedCase === "baseline failure") {
      await assert.rejects(produceMutationBaseline(planDir, baselineDir), /execution.*failure/);
      assert.equal(existsSync(join(baselineDir, "receipt.json")), false, "failed baseline must not publish a receipt");
      await assert.rejects(runMutationWorker(planDir, baselineDir, join(evidence, "no-baseline"), 0));
      return;
    }
    const baseline = await produceMutationBaseline(planDir, baselineDir);
    Object.assign(process.env, { MUTATION_BASELINE_DIGEST: evidenceDigest(baseline), MUTATION_BASELINE_ATTEMPT: "1", GITHUB_RUN_ATTEMPT: "2", ImageVersion: "different-unused-runner-image" });
    const worker = await runMutationWorker(planDir, baselineDir, join(evidence, "worker"), 0);
    assert.equal(worker.report.outcomes.some(item => item.scenario === "Baseline"), false);
    assert.equal(worker.summary[sharedCase], worker.selected.length);
    assert.deepEqual(JSON.parse(readFileSync(join(evidence, "worker/raw/mutants.out/outcomes.json"), "utf8")), worker.report);
    assert.equal(existsSync(join(root, "mutants.out")), false, "reports must never be written into the unignored source checkout");
    assert.equal(execFileSync("git", ["status", "--porcelain", "--untracked-files=normal"], { cwd: root, encoding: "utf8" }), "");
    const sourcePath = join(cwd, "src/lib.rs"), originalSource = readFileSync(sourcePath);
    writeFileSync(sourcePath, `${originalSource}\n// genuinely dirty source\n`);
    await assert.rejects(runMutationWorker(planDir, baselineDir, join(evidence, "dirty-source"), 0), /source must be clean/);
    writeFileSync(sourcePath, originalSource);
    const untracked = join(root, "unexpected-source.rs");
    writeFileSync(untracked, "untracked source");
    await assert.rejects(runMutationWorker(planDir, baselineDir, join(evidence, "untracked-source"), 0), /source must be clean/);
    rmSync(untracked);
    await assert.rejects(runMutationWorker(planDir, baselineDir, join(root, "evidence/inside-source"), 0), /evidence must be outside/);
    const originalExecution = readFileSync(join(baselineDir, "execution.jsonl"));
    writeFileSync(join(baselineDir, "execution.jsonl"), "changed execution");
    await assert.rejects(runMutationWorker(planDir, baselineDir, join(evidence, "changed-execution"), 0), /execution digest/);
    assert.equal(existsSync(join(evidence, "changed-execution/selection.log")), false, "invalid baseline must reject before listing or mutation execution");
    writeFileSync(join(baselineDir, "execution.jsonl"), originalExecution);
    process.env.MUTATION_BASELINE_ATTEMPT = "2";
    await assert.rejects(runMutationWorker(planDir, baselineDir, join(evidence, "rejected"), 0), /attempt mismatch/);
  } finally {
    process.chdir(previous);
    for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key];
    Object.assign(process.env, saved); rmSync(root, { recursive: true, force: true }); rmSync(evidence, { recursive: true, force: true });
  }
});

}

test("real portable cargo-mutants writes external reports without dirtying an unignored checkout", { timeout: 60000 }, async () => {
  const { execFileSync } = await import("node:child_process");
  const { mutationOptions } = await import("./mutation-baseline.mjs");
  const root = mkdtempSync(join(tmpdir(), "mesh-mutation-external-output-"));
  const evidence = mkdtempSync(join(tmpdir(), "mesh-mutation-external-reports-"));
  try {
    mkdirSync(join(root, "src")); mkdirSync(join(root, ".config"));
    writeFileSync(join(root, ".gitignore"), "/target/\n");
    writeFileSync(join(root, "Cargo.toml"), '[package]\nname="mutation-external-output"\nversion="0.1.0"\nedition="2024"\n[workspace]\n[profile.mutants]\ninherits="test"\n');
    writeFileSync(join(root, ".config/nextest.toml"), '[profile.mutants]\ntest-threads=4\n');
    writeFileSync(join(root, "src/lib.rs"), 'pub fn complete()->bool { true }\n#[test] fn verifies() { assert!(complete()); }\n');
    execFileSync("cargo", ["generate-lockfile", "--offline"], { cwd: root, timeout: 10000 });
    for (const args of [["init", "-q"], ["config", "user.name", "fixture"], ["config", "user.email", "fixture@example.invalid"], ["config", "core.hooksPath", "empty-hooks"], ["add", "."], ["commit", "-qm", "fixture"]]) execFileSync("git", args, { cwd: root, timeout: 10000 });
    const result = spawnSync("cargo", ["mutants", ...mutationOptions("0/256", evidence)], { cwd: root, encoding: "utf8", timeout: 40000, env: { ...process.env, CARGO_TARGET_DIR: join(root, "target") } });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stdout + result.stderr);
    const report = JSON.parse(readFileSync(join(evidence, "mutants.out/outcomes.json"), "utf8"));
    assert.equal(report.caught, 1);
    assert.equal(existsSync(join(root, "mutants.out")), false);
    assert.equal(execFileSync("git", ["status", "--porcelain", "--untracked-files=normal"], { cwd: root, encoding: "utf8", timeout: 10000 }), "");
  } finally { rmSync(root, { recursive: true, force: true }); rmSync(evidence, { recursive: true, force: true }); }
});

test("real pinned round-robin listings preserve the complete original sample across bounded chunks", { timeout: 60000 }, async () => {
  const { mutationOptions } = await import("./mutation-baseline.mjs");
  const { createMutationPlan, parseSelectedMutants } = await import("./mutation-evidence.mjs");
  const root = mkdtempSync(join(tmpdir(), "mesh-mutation-partitions-"));
  try {
    mkdirSync(join(root, "src"));
    writeFileSync(join(root, "Cargo.toml"), '[package]\nname="mutation-partitions"\nversion="0.1.0"\nedition="2024"\n[workspace]\n[profile.mutants]\ninherits="test"\n');
    writeFileSync(join(root, "src/lib.rs"), Array.from({ length: 600 }, (_, index) => `pub fn eligible_${index}(x:u32)->bool { x>10 }`).join("\n"));
    const lock = spawnSync("cargo", ["generate-lockfile", "--offline"], { cwd: root, encoding: "utf8" });
    assert.equal(lock.status, 0, lock.stderr);
    const list = shard => {
      const result = spawnSync("cargo", ["mutants", "--list", "--json", ...mutationOptions(shard)], { cwd: root, encoding: "utf8", timeout: 15000, maxBuffer: 32 * 1024 * 1024 });
      assert.ifError(result.error);
      return parseSelectedMutants(result.stdout, result.status);
    };
    const selected = list("0/256");
    assert.ok(selected.length > 4, "fixture must exercise more than one bounded worker");
    const plan = createMutationPlan(1, selected);
    for (const worker of plan.workers) assert.deepEqual(list(worker.shard), worker.selected, "actual tool partition must equal the sealed bounded plan");
  } finally { rmSync(root, { recursive: true, force: true }); }
});
