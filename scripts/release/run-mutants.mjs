import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { mutationOptions } from "./mutation-baseline.mjs";
import { mutationShard, parseSelectedMutants, validateMutationReport, requireMutationGate } from "./mutation-evidence.mjs";

const args = process.argv.slice(2);
if (["--plan", "--baseline", "--worker", "--gate"].includes(args[0])) {
  const mode = args.shift();
  if (mode === "--gate") {
    assert.equal(args.length, 0, "gate takes no paths");
    assert.ok(["true", "false"].includes(process.env.DEEP), "deep event verdict is required");
    requireMutationGate({ contracts: process.env.CONTRACTS, deep: process.env.DEEP === "true", plan: process.env.PLAN, baseline: process.env.BASELINE, workers: process.env.SHARDS, selected: process.env.SELECTED });
  } else {
    const owner = await import("./mutation-baseline.mjs");
    if (mode === "--plan") {
      assert.equal(args.length, 1, "plan needs one output directory");
      await owner.produceMutationPlan(resolve(args[0]));
    } else if (mode === "--baseline") {
      assert.equal(args.length, 2, "baseline needs plan and output directories");
      await owner.produceMutationBaseline(resolve(args[0]), resolve(args[1]));
    } else {
      assert.equal(args.length, 4, "worker needs plan, baseline, output and worker index");
      await owner.runMutationWorker(resolve(args[0]), resolve(args[1]), resolve(args[2]), Number(args[3]));
    }
  }
  process.exit(0);
}
const listOnly = args[0] === "--list-only";
if (listOnly) args.shift();
assert.ok(args.length === 0 || (args.length === 2 && args[0] === "--in-diff"), "unsupported mutation filter arguments");
const shard = mutationShard(Number(process.env.GITHUB_RUN_NUMBER ?? "1"), Number(process.env.MUTATION_WORKER ?? "0"), Number(process.env.MUTATION_WORKERS ?? "1"));
// Inline callers retain their own real baseline; shared CI workers use only the
// independently verified external mode above. Both keep all workspace consumers.
const options = [...mutationOptions(shard), ...args];
// rust-cache exports CARGO_INCREMENTAL=0 through GITHUB_ENV. Pin the actual
// child environment so listing and mutation builds retain incremental reuse.
const childEnv = { ...process.env, CARGO_INCREMENTAL: "1" };
const listing = spawnSync("cargo", ["mutants", "--list", "--json", ...options], { env: childEnv, encoding: "utf8", maxBuffer: 32 * 1024 * 1024 });
if (listing.error) throw listing.error;
assert.equal(listing.status, 0, `mutation list failed: ${listing.stderr}`);
const selected = parseSelectedMutants(listing.stdout, listing.status);
if (listOnly) {
  console.log(`Mutation sample ${shard}: ${selected.length} selected`);
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `selected=${selected.length}\n`);
  process.exit(0);
}
const reportPath = resolve("../mutants.out/outcomes.json");
let exitCode = 0;
if (selected.length > 0) {
  const result = spawnSync("cargo", ["mutants", ...options, "--", "--test-threads=4"], { env: childEnv, stdio: "inherit" });
  if (result.error) throw result.error;
  exitCode = result.status;
}
const report = existsSync(reportPath) ? JSON.parse(readFileSync(reportPath, "utf8")) : null;
const summary = validateMutationReport(selected.length, report, exitCode);
const text = `Mutation sample ${shard}: ${JSON.stringify(summary)}\nSurvivors are advisory; unviable mutants are not caught.\n`;
console.log(text);
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, text);
if (summary.missed > 0) console.log(`::warning::${summary.missed} surviving mutations need review; see mutants-report.`);
