import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { validateReport, validateEvidence, REQUIRED_SCENARIOS } from "./hidden-contacts-report.mjs";

const complete = () => ({
  schema: 2, sourceSha: "a".repeat(40), platform: process.platform, native: true, mocked: false,
  scenarios: Object.fromEntries(REQUIRED_SCENARIOS.map(name => [name, { passed: true, elapsedMs: 1, evidence: [`${name}.json`, `${name}.png`, `${name}.log`], evidenceDigests: Object.fromEntries(["json", "png", "log"].map(extension => [`${name}.${extension}`, "b".repeat(64)])) }])),
});
test("requires native invisible-mode, reply and restart evidence", () => {
  for (const name of ["privacy-mode", "privacy-reply", "privacy-restart"]) {
    assert.ok(REQUIRED_SCENARIOS.includes(name), `missing ${name}`);
    const report = complete();
    delete report.scenarios[name];
    assert.ok(validateReport(report).some(error => error.includes(name)));
  }
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
test("requires expanded core scenarios, exact source revision and evidence hashes", () => {
  for (const name of ["auth-register-signin", "direct-messages", "incoming-attachment", "native-profile-layout", "history-process-restart"]) assert.ok(REQUIRED_SCENARIOS.includes(name));
  for (const sourceSha of [undefined, "main", "0".repeat(39)]) assert.ok(validateReport({ ...complete(), sourceSha }).length);
  assert.ok(validateReport(complete(), { sourceSha: "c".repeat(40) }).length);
  assert.ok(validateReport(complete(), { platform: "invalid" }).length);
  const report = complete(); report.scenarios.hide.evidenceDigests["hide.png"] = "invalid";
  assert.ok(validateReport(report).length);
});
test("typed observations reject missing checks and preserve explicit download limitations", async () => {
  const { validateObservations } = await import("./hidden-contacts-report.mjs");
  assert.deepEqual(validateObservations("direct-messages", { uiToCliRendered: true, uiToCliPersisted: true, cliToUiRendered: true, cliToUiPersisted: true }), []);
  for (const observations of [{}, { uiToCliRendered: "true" }, { uiToCliRendered: false }]) assert.ok(validateObservations("direct-messages", observations).length);
  assert.deepEqual(validateObservations("incoming-attachment", { fileName: "fixture.txt", manifestPersisted: true, genericFileRendered: true, downloadVerified: false, limitation: "OS save picker is not automated" }), []);
  assert.ok(validateObservations("incoming-attachment", { manifestPersisted: true, genericFileRendered: true, downloadVerified: false }).length);
});
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
