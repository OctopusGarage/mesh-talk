// Retire redundant check names only after all replacement checks pass on the
// explicitly selected PR head. Preserve other checks, strictness and app IDs.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { migrateRequiredChecks, requireProtectionEvidence } from "./required-checks.mjs";

const [repo, branch, pr, mode, ...extra] = process.argv.slice(2);
assert.match(repo ?? "", /^[\w.-]+\/[\w.-]+$/);
assert.match(branch ?? "", /^[\w.-]+$/);
assert.match(pr ?? "", /^[1-9]\d*$/);
assert.ok(["--plan", "--apply"].includes(mode) && !extra.length, "Usage: migrate-required-checks.mjs REPO BRANCH PR --plan|--apply");
const api = (path, args = []) => JSON.parse(execFileSync("gh", ["api", path, ...args], { encoding: "utf8" }));
const endpoint = `repos/${repo}/branches/${branch}/protection/required_status_checks`;
const before = api(endpoint);
const next = migrateRequiredChecks(before);
console.log(JSON.stringify({ strict: next.strict, removed: before.checks.filter(check => !next.checks.some(value => value.context === check.context)), added: next.checks.filter(check => !before.checks.some(value => value.context === check.context)) }, null, 2));
if (mode === "--apply") {
  const candidate = api(`repos/${repo}/pulls/${pr}`);
  assert.equal(candidate.state, "open", "migration candidate must be an open PR");
  assert.equal(candidate.base.ref, branch, "candidate targets another branch");
  assert.equal(candidate.head.repo.full_name, repo, "migration candidate must belong to this repository");
  const pages = api(`repos/${repo}/commits/${candidate.head.sha}/check-runs?per_page=100&filter=all`, ["--paginate", "--slurp"]);
  requireProtectionEvidence(next, pages.flatMap(page => page.check_runs));
  // Do not overwrite a concurrent administrator or source change.
  const current = api(endpoint);
  assert.deepEqual({ strict: current.strict, checks: current.checks }, { strict: before.strict, checks: before.checks }, "branch protection changed during migration");
  assert.equal(api(`repos/${repo}/pulls/${pr}`).head.sha, candidate.head.sha, "candidate advanced during migration");
  execFileSync("gh", ["api", endpoint, "--method", "PATCH", "--input", "-"], { input: JSON.stringify(next), stdio: ["pipe", "ignore", "inherit"] });
  const actual = api(endpoint);
  assert.deepEqual({ strict: actual.strict, checks: [...actual.checks].sort((a, b) => a.context.localeCompare(b.context, "en")) }, next, "protection readback mismatch");
  console.log(`Migrated ${repo}:${branch} after app-bound checks passed on ${candidate.head.sha}`);
}
