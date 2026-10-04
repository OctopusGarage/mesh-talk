import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { parseSelectedMutants, validateMutationReport } from "./mutation-evidence.mjs";

const args = process.argv.slice(2);
assert.ok(args.length === 0 || (args.length === 2 && args[0] === "--in-diff"), "unsupported mutation filter arguments");
const options = ["--workspace", "--test-workspace", "true", "--no-shuffle", "--output", "..", "-j", "2", ...args];
const listing = spawnSync("cargo", ["mutants", "--list", "--json", ...options], { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 });
if (listing.error) throw listing.error;
assert.equal(listing.status, 0, `mutation list failed: ${listing.stderr}`);
const selected = parseSelectedMutants(listing.stdout, listing.status);
const reportPath = resolve("../mutants.out/outcomes.json");
let exitCode = 0;
if (selected.length > 0) {
  const result = spawnSync("cargo", ["mutants", ...options, "--", "--", "--test-threads=2"], { stdio: "inherit" });
  if (result.error) throw result.error;
  exitCode = result.status;
}
const report = existsSync(reportPath) ? JSON.parse(readFileSync(reportPath, "utf8")) : null;
const summary = validateMutationReport(selected.length, report, exitCode);
const text = `Mutation evidence: ${JSON.stringify(summary)}\nSurvivors are advisory; unviable mutants are not caught.\n`;
console.log(text);
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, text);
if (summary.missed > 0) console.log(`::warning::${summary.missed} surviving mutations need review; see mutants-report.`);
