import assert from "node:assert/strict";
import { test } from "node:test";
import { readFile } from "node:fs/promises";

test("health installs the headless runtime it actually tests and surfaces setup failure", async () => {
  const source = await readFile(new URL("../check-health.sh", import.meta.url), "utf8");
  assert.ok(/playwright install --only-shell chromium/.test(source), "headless runtime should be explicit");
  assert.ok(!/playwright install[^\n]+\|\| true/.test(source), "setup failure must not be swallowed");
  assert.ok(/Chromium runtime installation failed/.test(source), "setup errors should have an actionable diagnostic");
});
test("health executes source and E2E type checking, not just transpilation", async () => {
  const source = await readFile(new URL("../check-health.sh", import.meta.url), "utf8");
  assert.ok(source.includes("npm run typecheck"), "typecheck must be part of the actual health gate");
});
test("health uses the CI worker budget without retrying failed UI scenarios", async () => {
  const source = await readFile(new URL("../check-health.sh", import.meta.url), "utf8");
  assert.match(source, /playwright test --project=chromium --workers=1 --retries=0/);
  const workflow = await readFile(new URL("../../.github/workflows/e2e-ui.yml", import.meta.url), "utf8");
  assert.ok(workflow.includes("npx playwright test --workers=1 --retries=0"), "baseline CI must not turn flaky failures into successful checks");
  const config = await readFile(new URL("../../frontend/playwright.config.ts", import.meta.url), "utf8");
  assert.ok(config.includes('trace: "retain-on-failure"'), "the first failure must retain a trace without requiring a retry");
});
