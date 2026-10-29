// Direct execution checks shared by release acceptance and the one-time protection
// migration. PR-only and main-only policies deliberately remain separate.
import assert from "node:assert/strict";

const windows = ["windows / Windows workspace build", "windows / Windows workspace tests (1/2)", "windows / Windows workspace tests (2/2)"];
const workspace = [...["ubuntu-latest", "macos-latest"].map(os => `checks (${os})`), ...windows];
const health = ["health / Source and supply-chain checks", ...["ubuntu-latest", "macos-latest", "windows-latest"].map(os => `health / Portable checks (${os})`)];
const backend = ["windows-2022", "macos-latest"].map(os => `Backend core (${os})`);
const native = ["ubuntu-24.04", "windows-2022", "macos-latest"].map(os => `native-core / Real native UI (${os})`);

export const releaseSourceChecks = Object.freeze([
  ...workspace, ...health, ...backend, ...native,
  "Playwright UI E2E", "scan", "Scorecard analysis",
  "Analyze (rust)", "Analyze (actions)", "Analyze (javascript-typescript)",
]);

export function pendingSuccessfulChecks(checks, names, appSlug = "github-actions") {
  assert.ok(Array.isArray(checks), "source checks must be an array");
  return names.filter(name => {
    const latest = checks.filter(check => check.name === name && check.app?.slug === appSlug).sort((a, b) => b.id - a.id)[0];
    if (!latest || latest.status !== "completed") return true;
    assert.equal(latest.conclusion, "success", `required source check not successful: ${name}`);
    return false;
  });
}

export function requireSuccessfulChecks(checks, names, appSlug) {
  const pending = pendingSuccessfulChecks(checks, names, appSlug);
  assert.equal(pending.length, 0, `required source check not successful: ${pending.join(", ")}`);
}

const replacements = new Map([
  ["verify", workspace],
  ["checks (windows-latest)", windows],
  ["Backend core (ubuntu-24.04)", ["checks (ubuntu-latest)"]],
  ["Multi-process E2E", ["checks (ubuntu-latest)", ...backend, ...native]],
  ...["ubuntu-latest", "macos-latest", "windows-latest"].map(os => [`Code Quality and Health Check (${os})`, health]),
  ...["ubuntu-24.04", "windows-2022", "macos-latest"].map(os => [`native-core / Runner multicast diagnostic (${os})`, [`native-core / Real native UI (${os})`]]),
  ...["ubuntu-24.04", "windows-2022", "macos-latest"].map(os => [`Portable UI (${os}, chromium)`, ["Chromium portable evidence"]]),
]);

export function migrateRequiredChecks(current) {
  assert.equal(typeof current.strict, "boolean", "protection strictness is required");
  assert.ok(Array.isArray(current.checks) && current.checks.length, "app-bound required checks are required");
  const migrated = new Map();
  for (const check of current.checks) {
    assert.ok(typeof check.context === "string" && Number.isInteger(check.app_id), "invalid required check app binding");
    for (const context of replacements.get(check.context) ?? [check.context]) {
      const prior = migrated.get(context);
      assert.ok(!prior || prior.app_id === check.app_id, `conflicting check app binding: ${context}`);
      migrated.set(context, { context, app_id: check.app_id });
    }
  }
  return { strict: current.strict, checks: [...migrated.values()].sort((a, b) => a.context.localeCompare(b.context, "en")) };
}

export function requireProtectionEvidence(policy, checks) {
  for (const { context, app_id: appId } of policy.checks) {
    const latest = checks.filter(check => check.name === context && check.app?.id === appId).sort((a, b) => b.id - a.id)[0];
    assert.ok(latest?.status === "completed" && latest.conclusion === "success", `required replacement check has no successful current-source app-bound evidence: ${context}`);
  }
}
