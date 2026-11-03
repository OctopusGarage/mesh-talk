import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { mutationShard, parseSelectedMutants, validateMutationReport } from "./mutation-evidence.mjs";

const args = process.argv.slice(2);
const listOnly = args[0] === "--list-only";
if (listOnly) args.shift();
assert.ok(args.length === 0 || (args.length === 2 && args[0] === "--in-diff"), "unsupported mutation filter arguments");
const shard = mutationShard(Number(process.env.GITHUB_RUN_NUMBER ?? "1"), Number(process.env.MUTATION_WORKER ?? "0"), Number(process.env.MUTATION_WORKERS ?? "1"));
// Keep four isolated test processes and the existing per-test hang detector.
// The pinned tool's package baseline has 2,228 tests, but each mutant runs all
// 2,829 workspace tests. Observed work on slower runners exceeds 240s even
// without a hung test; bound the full inventory separately at six minutes.
// In-place mode enforces one worker, reusing the checkout's build cache. CI runs
// shards on separate machines to avoid competing workspace builds and tests.
const options = ["--test-tool", "nextest", "--cargo-test-arg=--profile=mutants", "--workspace", "--test-workspace", "true", "--in-place", "--profile", "mutants", "--no-shuffle", "--shard", shard, "--sharding", "round-robin", "--timeout", "360", "--output", "..", ...args];
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
