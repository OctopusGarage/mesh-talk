import assert from "node:assert/strict";
import test from "node:test";
import { releaseInputs } from "../release/preflight.mjs";
import { requireSourceChecks } from "../release/check-source-checks.mjs";

const inputs = { ref: "refs/tags/v0.1.6", runId: "123", version: "0.1.6", desktopVersion: "0.1.6", existingRelease: null };

test("preflight enforces matching versions and never overwrites public releases", () => {
  assert.deepEqual(releaseInputs(inputs), { version: "0.1.6", tag: "v0.1.6" });
  assert.throws(() => releaseInputs({ ...inputs, desktopVersion: "0.1.5" }), /version/);
  assert.throws(() => releaseInputs({ ...inputs, ref: "refs/tags/v0.1.5" }), /version/);
  assert.throws(() => releaseInputs({ ...inputs, existingRelease: { draft: false } }), /published/);
  assert.throws(() => releaseInputs({ ...inputs, existingRelease: {} }), /published/);
  assert.deepEqual(releaseInputs({ ...inputs, existingRelease: { draft: true } }), { version: "0.1.6", tag: "v0.1.6" });
});

test("branch dry runs use safe unique artifact names, not slash-containing branch names", () => {
  assert.deepEqual(releaseInputs({ ...inputs, ref: "refs/heads/chore/quality-gates" }), { version: "0.1.6", tag: "v0.1.6-ci.123" });
  assert.throws(() => releaseInputs({ ...inputs, ref: "refs/pull/1/merge" }), /ref/);
  assert.throws(() => releaseInputs({ ...inputs, runId: "../../bad" }), /run/);
  assert.throws(() => releaseInputs({ ...inputs, ref: "refs/heads/dev", publishRelease: true }), /publication/);
});

const names = ["verify", "Playwright UI E2E", "Multi-process E2E", "Code Quality and Health Check (macos-latest)", "Code Quality and Health Check (windows-latest)", "scan", "Scorecard analysis", "Analyze (rust)", "Analyze (actions)", "Analyze (javascript-typescript)"];
const checks = names.map((name, id) => ({ name, id, app: { slug: "github-actions" }, status: "completed", conclusion: "success" }));

test("publication requires successful current-source CI and all CodeQL languages", () => {
  requireSourceChecks(checks, []);
  for (const missing of names) assert.throws(() => requireSourceChecks(checks.filter((check) => check.name !== missing), []), /check/);
  for (const conclusion of ["failure", "cancelled", "skipped", "neutral", null]) assert.throws(() => requireSourceChecks([{ ...checks[0], conclusion }, ...checks.slice(1)], []), /check/);
  assert.throws(() => requireSourceChecks([{ ...checks[0], status: "in_progress" }, ...checks.slice(1)], []), /check/);
  assert.throws(() => requireSourceChecks(checks, [{ number: 1 }]), /alerts/);
});

test("a stale success or third-party check cannot mask a newer failed run", () => {
  assert.throws(() => requireSourceChecks([...checks, { ...checks[0], id: 100, conclusion: "failure" }], []), /check/);
  assert.throws(() => requireSourceChecks([{ ...checks[0], app: { slug: "other" } }, ...checks.slice(1)], []), /check/);
});
