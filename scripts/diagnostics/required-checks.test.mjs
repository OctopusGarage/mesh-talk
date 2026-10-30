import assert from "node:assert/strict";
import test from "node:test";
import { migrateRequiredChecks, requireSuccessfulChecks, releaseSourceChecks } from "../ci/required-checks.mjs";

test("gate migration preserves strictness, app binding and unrelated protection", () => {
  const current = { strict: true, checks: [
    { context: "verify", app_id: 15368 },
    { context: "Code Quality and Health Check (windows-latest)", app_id: 15368 },
    { context: "codecov/patch", app_id: 254 },
    { context: "review", app_id: 42 },
  ] };
  const next = migrateRequiredChecks(current);
  assert.equal(next.strict, true);
  assert.deepEqual(next.checks.find(x => x.context === "codecov/patch"), current.checks[2]);
  assert.deepEqual(next.checks.find(x => x.context === "review"), current.checks[3]);
  for (const name of ["checks (ubuntu-latest)", "checks (macos-latest)", "windows / Windows workspace build", "windows / Windows workspace tests (1/2)", "windows / Windows workspace tests (2/2)", "health / Source and supply-chain checks", "health / Portable checks (windows-latest)"]) {
    assert.deepEqual(next.checks.find(x => x.context === name), { context: name, app_id: 15368 });
  }
  assert.ok(!next.checks.some(x => x.context === "verify"));
  assert.deepEqual(migrateRequiredChecks(next), next);
  assert.throws(() => migrateRequiredChecks({ strict: true, checks: [current.checks[0], { context: "checks (ubuntu-latest)", app_id: 666 }] }), /app/);
});

test("direct release acceptance rejects every missing, skipped, failed or spoofed source check", () => {
  const checks = releaseSourceChecks.map((name, id) => ({ name, id, app: { slug: "github-actions" }, status: "completed", conclusion: "success" }));
  requireSuccessfulChecks(checks, releaseSourceChecks);
  for (const name of releaseSourceChecks) assert.throws(() => requireSuccessfulChecks(checks.filter(x => x.name !== name), releaseSourceChecks), /check/);
  for (const conclusion of ["failure", "skipped", "neutral", "cancelled", null]) assert.throws(() => requireSuccessfulChecks([{ ...checks[0], id: 999, conclusion }, ...checks], releaseSourceChecks), /check/);
  assert.throws(() => requireSuccessfulChecks([{ ...checks[0], app: { slug: "other" } }, ...checks.slice(1)], releaseSourceChecks), /check/);
  assert.ok(!releaseSourceChecks.includes("verify"));
  for (const name of ["windows / Windows workspace build", "windows / Windows workspace tests (1/2)", "windows / Windows workspace tests (2/2)"]) assert.ok(releaseSourceChecks.includes(name));
  assert.ok(releaseSourceChecks.includes("native-core / Real native UI (windows-2022)"));
});

test("protection migration requires current-source successful checks from their bound app", async () => {
  const { requireProtectionEvidence } = await import("../ci/required-checks.mjs");
  const policy = { checks: [{ context: "native", app_id: 15368 }, { context: "codecov/patch", app_id: 254 }] };
  const checks = policy.checks.map((entry, id) => ({ name: entry.context, app: { id: entry.app_id }, id, status: "completed", conclusion: "success" }));
  requireProtectionEvidence(policy, checks);
  for (const alter of [c => c.pop(), c => { c[0].status = "queued"; }, c => { c[0].conclusion = "skipped"; }, c => { c[1].app.id = 15368; }, c => c.push({ ...c[0], id: 99, conclusion: "failure" })]) {
    const bad = structuredClone(checks); alter(bad);
    assert.throws(() => requireProtectionEvidence(policy, bad), /check/);
  }
});

test("migrating the backend aggregate alone preserves Linux execution acceptance", () => {
  const next = migrateRequiredChecks({ strict: true, checks: [{ context: "Multi-process E2E", app_id: 15368 }] });
  assert.ok(next.checks.some(check => check.context === "checks (ubuntu-latest)"));
  assert.equal(next.checks.length, 6);
});

test("the former Windows matrix check requires build and both executable archive partitions", () => {
  const policy = migrateRequiredChecks({ strict: true, checks: [{ context: "checks (windows-latest)", app_id: 15368 }] });
  assert.deepEqual(policy.checks.map(check => check.context), ["windows / Windows workspace build", "windows / Windows workspace tests (1/2)", "windows / Windows workspace tests (2/2)"]);
  assert.ok(policy.checks.every(check => check.app_id === 15368));
});

test("Mac capacity allocation migrates Intel work while preserving ARM acceptance", () => {
  const oldNames = ["health / Portable checks (macos-latest)", "Backend core (macos-latest)"];
  const armNames = ["checks (macos-latest)", "native-core / Real native UI (macos-latest)"];
  const webkit = "Portable UI (macos-latest, webkit)";
  const policy = migrateRequiredChecks({ strict: true, checks: [...oldNames, ...armNames, webkit].map(context => ({ context, app_id: 15368 })) });
  const names = policy.checks.map(check => check.context);
  for (const name of oldNames) {
    assert.ok(!names.includes(name));
    assert.ok(names.includes(name.replace("macos-latest", "macos-26-intel")));
  }
  for (const name of armNames) assert.ok(names.includes(name));
  assert.ok(names.includes(webkit));
  assert.ok(!names.includes(webkit.replace("macos-latest", "macos-26-intel")));
  for (const name of ["health / Portable checks (macos-26-intel)", "Backend core (macos-26-intel)", ...armNames]) assert.ok(releaseSourceChecks.includes(name));
  assert.deepEqual(migrateRequiredChecks(policy), policy);
});
