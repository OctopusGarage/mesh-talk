import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { pathToFileURL } from "node:url";

function pendingSourceChecks(checks, openAlerts) {
  const pending = [];
  assert.ok(Array.isArray(openAlerts) && openAlerts.length === 0, "main has unresolved code scanning alerts");
  for (const name of ["verify", "Playwright UI E2E", "Multi-process E2E", "Code Quality and Health Check (macos-latest)", "Code Quality and Health Check (windows-latest)", "scan", "Scorecard analysis", "Analyze (rust)", "Analyze (actions)", "Analyze (javascript-typescript)"]) {
    const latest = checks.filter((check) => check.name === name && check.app?.slug === "github-actions").sort((a, b) => b.id - a.id)[0];
    if (!latest || latest.status !== "completed") pending.push(name);
    else assert.equal(latest.conclusion, "success", `required source check not successful: ${name}`);
  }
  return pending;
}

export function requireSourceChecks(checks, openAlerts) {
  const pending = pendingSourceChecks(checks, openAlerts);
  assert.equal(pending.length, 0, `required source check not successful: ${pending.join(", ")}`);
}

// Only Nightly preflight waits. Final publication and Stable callers remain strict.
export async function waitForSourceChecks(readState, { timeoutMs = 4 * 60 * 60 * 1000, intervalMs = 60_000, now = Date.now, sleep: pause = sleep, log = console.log } = {}) {
  const deadline = now() + timeoutMs;
  for (;;) {
    const { checks, openAlerts } = await readState();
    const pending = pendingSourceChecks(checks, openAlerts);
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
    return { checks: pages.flatMap((page) => page.check_runs), openAlerts: alerts.flat() };
  };
  if (mode === "--wait") await waitForSourceChecks(readState);
  else {
    const { checks, openAlerts } = readState();
    requireSourceChecks(checks, openAlerts);
  }
  console.log("Current-source CI, health, E2E and security checks passed; main has no open scanning alerts.");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main();
