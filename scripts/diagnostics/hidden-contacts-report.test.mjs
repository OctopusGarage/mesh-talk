import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { validateReport, validateEvidence, REQUIRED_SCENARIOS } from "./hidden-contacts-report.mjs";

const complete = () => ({
  schema: 1, platform: process.platform, native: true, mocked: false,
  scenarios: Object.fromEntries(REQUIRED_SCENARIOS.map(name => [name, { passed: true, elapsedMs: 1, evidence: [`${name}.json`, `${name}.png`] }])),
});
test("rejects absent screenshots, path traversal and malformed timings", () => {
  for (const patch of [{ evidence: ["hide.json"] }, { evidence: ["../hide.json", "hide.png"] }, { elapsedMs: undefined }, { elapsedMs: -1 }]) {
    const report = complete();
    Object.assign(report.scenarios.hide, patch);
    assert.ok(validateReport(report).length);
  }
});
test("requires evidence files to exist and contain valid JSON and PNG data", async () => {
  const root = await mkdtemp(join(tmpdir(), "native-contact-evidence-"));
  const report = complete();
  assert.ok((await validateEvidence(report, root)).length);
  for (const name of REQUIRED_SCENARIOS) {
    await writeFile(join(root, `${name}.json`), "not json");
    await writeFile(join(root, `${name}.png`), "not an image");
  }
  assert.ok((await validateEvidence(report, root)).length);
});
test("accepts a complete native evaluation", () => assert.deepEqual(validateReport(complete()), []));
test("rejects missing, failed and evidence-free scenarios", () => {
  for (const value of [undefined, { passed: false, evidence: ["x"] }, { passed: true, evidence: [] }]) {
    const report = complete();
    report.scenarios[REQUIRED_SCENARIOS[0]] = value;
    assert.ok(validateReport(report).length);
  }
});
test("rejects renderer mocks and unsupported platforms", () => {
  for (const patch of [{ native: false }, { mocked: true }, { platform: "unknown" }, { schema: 0 }, { failure: "driver died" }, { cleanupFailure: "child remained alive" }]) {
    assert.ok(validateReport({ ...complete(), ...patch }).length);
  }
});
