import assert from "node:assert/strict";
import { pendingSuccessfulChecks, releaseSourceChecks, releaseSourceWorkflows } from "../ci/required-checks.mjs";
import { execFileSync } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { pathToFileURL } from "node:url";

export function pendingSourceChecks(checks, openAlerts, { repo, sha, runs } = {}) {
  assert.ok(Array.isArray(openAlerts) && openAlerts.length === 0, "main has unresolved code scanning alerts");
  assert.match(repo ?? "", /^[\w.-]+\/[\w.-]+$/, "source repository is required");
  assert.match(sha ?? "", /^[a-f\d]{40}$/, "source SHA is required");
  assert.ok(Array.isArray(runs), "source workflow runs are required");
  assert.ok(Array.isArray(checks), "source checks must be an array");
  // Preserve the latest same-source check veto across all suites. Trusted main
  // provenance adds evidence requirements; it never hides a newer failure.
  const pending = pendingSuccessfulChecks(checks, releaseSourceChecks);
  for (const path of new Set(Object.values(releaseSourceWorkflows))) {
    const latest = runs.filter(run => run.path === path && run.event === "push" && run.head_branch === "main"
      && run.head_sha === sha && run.repository?.full_name === repo && run.head_repository?.full_name === repo)
      .sort((a, b) => b.id - a.id || b.run_attempt - a.run_attempt)[0];
    const names = releaseSourceChecks.filter(name => releaseSourceWorkflows[name] === path);
    if (!latest || latest.status !== "completed") {
      pending.push(...names);
      continue;
    }
    assert.ok(Number.isSafeInteger(latest.id) && latest.id > 0
      && Number.isSafeInteger(latest.run_attempt) && latest.run_attempt > 0
      && Number.isSafeInteger(latest.check_suite_id) && latest.check_suite_id > 0, "invalid source workflow identity");
    assert.equal(latest.conclusion, "success", `required source workflow not successful: ${path}`);
    // A successful failed-jobs-only rerun retains earlier successful jobs in this
    // suite. The completed latest attempt attests the suite without requiring
    // every successful job to execute again.
    pending.push(...pendingSuccessfulChecks(checks.filter(check => check.check_suite?.id === latest.check_suite_id), names));
  }
  return [...new Set(pending)];
}

export function requireSourceChecks(checks, openAlerts, provenance) {
  const pending = pendingSourceChecks(checks, openAlerts, provenance);
  assert.equal(pending.length, 0, `required source check not successful: ${pending.join(", ")}`);
}

// Only Nightly preflight waits. Final publication and Stable callers remain strict.
export async function waitForSourceChecks(readState, { timeoutMs = 4 * 60 * 60 * 1000, intervalMs = 60_000, now = Date.now, sleep: pause = sleep, log = console.log } = {}) {
  const deadline = now() + timeoutMs;
  for (;;) {
    const state = await readState();
    const pending = pendingSourceChecks(state.checks, state.openAlerts, state);
    if (pending.length === 0) return;
    const remaining = deadline - now();
    assert.ok(remaining > 0, `Timed out waiting for required source checks: ${pending.join(", ")}`);
    log(`Waiting for required source checks: ${pending.join(", ")} (${Math.ceil(remaining / 60_000)} minutes remaining)`);
    await pause(Math.min(intervalMs, remaining));
  }
}

async function main() {
  const [repo, sha, mode, ...extra] = process.argv.slice(2);
  assert.ok((mode === undefined || mode === "--wait") && extra.length === 0, "usage: check-source-checks.mjs REPO SHA [--wait]");
  assert.match(repo, /^[\w.-]+\/[\w.-]+$/);
  assert.match(sha, /^[a-f\d]{40}$/);
  const readState = () => {
    const pages = JSON.parse(execFileSync("gh", ["api", "--paginate", "--slurp", `repos/${repo}/commits/${sha}/check-runs?per_page=100&filter=all`], { encoding: "utf8" }));
    const alerts = JSON.parse(execFileSync("gh", ["api", "--paginate", "--slurp", `repos/${repo}/code-scanning/alerts?state=open&ref=refs%2Fheads%2Fmain&per_page=100`], { encoding: "utf8" }));
    const runPages = JSON.parse(execFileSync("gh", ["api", "--paginate", "--slurp", `repos/${repo}/actions/runs?head_sha=${sha}&per_page=100`], { encoding: "utf8" }));
    return { repo, sha, runs: runPages.flatMap(page => page.workflow_runs), checks: pages.flatMap((page) => page.check_runs), openAlerts: alerts.flat() };
  };
  if (mode === "--wait") await waitForSourceChecks(readState);
  else {
    const state = readState();
    requireSourceChecks(state.checks, state.openAlerts, state);
  }
  console.log("Current-source CI, health, E2E and security checks passed; main has no open scanning alerts.");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main();
