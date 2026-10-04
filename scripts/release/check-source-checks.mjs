import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

export function requireSourceChecks(checks, openAlerts) {
  assert.ok(Array.isArray(openAlerts) && openAlerts.length === 0, "main has unresolved code scanning alerts");
  for (const name of ["verify", "Playwright UI E2E", "Multi-process E2E", "Code Quality and Health Check (macos-latest)", "Code Quality and Health Check (windows-latest)", "scan", "Scorecard analysis", "Analyze (rust)", "Analyze (actions)", "Analyze (javascript-typescript)"]) {
    const latest = checks.filter((check) => check.name === name && check.app?.slug === "github-actions").sort((a, b) => b.id - a.id)[0];
    assert.ok(latest?.status === "completed" && latest.conclusion === "success", `required source check not successful: ${name}`);
  }
}

function main() {
  const [repo, sha] = process.argv.slice(2);
  assert.match(repo, /^[\w.-]+\/[\w.-]+$/);
  assert.match(sha, /^[a-f\d]{40}$/);
  const pages = JSON.parse(execFileSync("gh", ["api", "--paginate", "--slurp", `repos/${repo}/commits/${sha}/check-runs?per_page=100&filter=all`], { encoding: "utf8" }));
  const alerts = JSON.parse(execFileSync("gh", ["api", "--paginate", "--slurp", `repos/${repo}/code-scanning/alerts?state=open&ref=refs%2Fheads%2Fmain&per_page=100`], { encoding: "utf8" }));
  requireSourceChecks(pages.flatMap((page) => page.check_runs), alerts.flat());
  console.log("Current-source CI, health, E2E and security checks passed; main has no open scanning alerts.");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
