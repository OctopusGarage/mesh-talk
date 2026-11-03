import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { findingIdentity, requireCleanCodeQL } from "../ci/codeql-results.mjs";

const gate = fileURLToPath(new URL("../ci/codeql-results.mjs", import.meta.url));
const clean = () => ({ version: "2.1.0", runs: [{ tool: { driver: { name: "CodeQL" } }, results: [] }] });
async function fixture(t, files) {
  const directory = await mkdtemp(path.join(tmpdir(), "mesh-codeql-results-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  for (const [name, report] of Object.entries(files)) await writeFile(path.join(directory, name), typeof report === "string" ? report : JSON.stringify(report));
  return directory;
}
const run = directory => spawnSync(process.execPath, [gate, directory], { encoding: "utf8", timeout: 5000 });

test("only the exact reviewed finding and data flow qualify", async t => {
  const finding = { ruleId: "fixture-rule", locations: [{ physicalLocation: { artifactLocation: { uri: "fixture.rs" }, region: { startLine: 1 } } }], partialFingerprints: { primaryLocationLineHash: "reviewed:1" }, codeFlows: [{ threadFlows: [{ locations: [{ location: { message: { text: "public identifier" }, physicalLocation: { artifactLocation: { uri: "fixture.rs" }, region: { startLine: 4 } } } }] }] }] };
  const directory = await fixture(t, { "fixture.rs": "sink();\nsetup();\nready();\npublic_identifier();\npublic_identifier();\n" });
  const identity = findingIdentity({}, finding, directory);
  const reviewed = [{ ...identity, alert: 1, review: "https://github.com/OctopusGarage/mesh-talk/security/code-scanning/1", reason: "An independently reviewed in-memory operation" }];
  const report = clean();
  report.runs[0].results = [finding];
  await writeFile(path.join(directory, "rust.sarif"), JSON.stringify(report));
  assert.equal(requireCleanCodeQL(directory, reviewed, directory).reviewedFindings, 1);
  // Unrelated preceding edits may move a finding without changing its evidence.
  const moved = structuredClone(finding);
  moved.codeFlows[0].threadFlows[0].locations[0].location.physicalLocation.region.startLine++;
  assert.deepEqual(findingIdentity({}, moved, directory), identity);
  for (const change of [
    changed => { changed.ruleId = "another-rule"; },
    changed => { changed.locations[0].physicalLocation.artifactLocation.uri = "another.rs"; },
    changed => { changed.partialFingerprints.primaryLocationLineHash = "new:1"; },
    changed => { changed.codeFlows[0].threadFlows[0].locations[0].location.message.text = "private key"; },
    changed => { delete changed.partialFingerprints; },
    changed => { delete changed.codeFlows[0].threadFlows[0].locations[0].location.physicalLocation.region; },
    changed => { changed.locations[0].physicalLocation.artifactLocation.uri = "../outside.rs"; },
    changed => { changed.locations.push(structuredClone(changed.locations[0])); },
  ]) {
    const changed = structuredClone(finding);
    change(changed);
    report.runs[0].results = [changed];
    await writeFile(path.join(directory, "rust.sarif"), JSON.stringify(report));
    assert.throws(() => requireCleanCodeQL(directory, reviewed, directory), /finding|fingerprint/);
  }
  report.runs[0].results = [finding];
  await writeFile(path.join(directory, "rust.sarif"), JSON.stringify(report));
  await writeFile(path.join(directory, "fixture.rs"), "sink();\nsetup();\nready();\nprivate_key();\npublic_identifier();\n");
  assert.throws(() => requireCleanCodeQL(directory, reviewed, directory), /changed CodeQL finding/);
});

test("two reviewed witnesses never admit a third unreviewed flow at the same sink", async t => {
  const directory = await fixture(t, {
    "fixture.rs": "sink();\npublic_identifier();\nowner_certificate();\nprivate_key();\n",
  });
  const finding = {
    ruleId: "fixture-rule",
    locations: [{ physicalLocation: { artifactLocation: { uri: "fixture.rs" }, region: { startLine: 1 } } }],
    partialFingerprints: { primaryLocationLineHash: "reviewed:1" },
    codeFlows: [{ threadFlows: [{ locations: [{ location: {
      message: { text: "public identifier" },
      physicalLocation: { artifactLocation: { uri: "fixture.rs" }, region: { startLine: 2 } },
    } }] }] }],
  };
  const owner = structuredClone(finding);
  owner.codeFlows[0].threadFlows[0].locations[0].location.message.text = "public owner certificate";
  owner.codeFlows[0].threadFlows[0].locations[0].location.physicalLocation.region.startLine = 3;
  const reviewed = [finding, owner].map(witness => ({
    ...findingIdentity({}, witness, directory), alert: 1,
    review: "https://github.com/OctopusGarage/mesh-talk/security/code-scanning/1",
    reason: "Each exact witness was independently reviewed; the sink is an in-memory operation",
  }));
  const report = clean();
  for (const witness of [finding, owner]) {
    report.runs[0].results = [witness];
    await writeFile(path.join(directory, "rust.sarif"), JSON.stringify(report));
    assert.equal(requireCleanCodeQL(directory, reviewed, directory).reviewedFindings, 1);
  }
  const unreviewed = structuredClone(owner);
  unreviewed.codeFlows[0].threadFlows[0].locations[0].location.message.text = "private key";
  unreviewed.codeFlows[0].threadFlows[0].locations[0].location.physicalLocation.region.startLine = 4;
  report.runs[0].results = [unreviewed];
  await writeFile(path.join(directory, "rust.sarif"), JSON.stringify(report));
  assert.throws(() => requireCleanCodeQL(directory, reviewed, directory), /unreviewed or changed/);
});

test("every CodeQL matrix analysis validates its actual local SARIF after upload", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/codeql.yml", import.meta.url), "utf8");
  const analysis = workflow.split(/^      - /m).find(step => step.startsWith("name: Perform CodeQL Analysis\n"));
  const acceptance = workflow.split(/^      - /m).find(step => step.startsWith("name: Reject CodeQL findings\n"));
  assert.ok(acceptance, "an upload-only analysis must not certify zero findings");
  assert.match(analysis, /id: analysis/);
  assert.match(acceptance, /steps\.analysis\.outputs\['sarif-output'\]/);
  assert.match(acceptance, /node scripts\/ci\/codeql-results\.mjs "\$CODEQL_SARIF_DIRECTORY"/);
  assert.doesNotMatch(acceptance, /continue-on-error:|\bif:/);
  assert.ok(workflow.indexOf("name: Perform CodeQL Analysis") < workflow.indexOf("name: Reject CodeQL findings"));
  assert.match(workflow, /language: \[rust, javascript-typescript, actions\]/);
  assert.match(workflow, /CODEQL_ACTION_DIFF_INFORMED_QUERIES: "false"/);
});

test("a completed clean CodeQL analysis passes with explicit zero findings", async t => {
  const result = run(await fixture(t, { "actions.sarif": clean() }));
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /0 unreviewed findings/);
});

for (const invocation of [
  { executionSuccessful: false },
  { executionSuccessful: true, toolExecutionNotifications: [{ level: "error", message: { text: "Incomplete extraction" } }] },
]) test("reviewed findings never excuse an analysis execution error", async t => {
  const report = clean();
  report.runs[0].invocations = [invocation];
  assert.notEqual(run(await fixture(t, { "actions.sarif": report })).status, 0);
});

test("an uploaded finding fails even when a different analysis is clean", async t => {
  const report = clean();
  report.runs[0].results.push({ ruleId: "js/bad-code-sanitization", level: "error", message: { text: "Generated code" } });
  const result = run(await fixture(t, { "actions.sarif": clean(), "javascript.sarif": report }));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /finding/);
});

for (const level of ["warning", "note"]) test(`CodeQL ${level} findings cannot bypass zero-alert acceptance`, async t => {
  const report = clean();
  report.runs[0].results.push({ ruleId: "fixture-rule", level, message: { text: "A finding" } });
  const result = run(await fixture(t, { "actions.sarif": report }));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /finding/);
});

for (const [name, files] of [
  ["missing SARIF", {}],
  ["malformed JSON", { "actions.sarif": "{" }],
  ["empty runs", { "actions.sarif": { version: "2.1.0", runs: [] } }],
  ["missing results", { "actions.sarif": { version: "2.1.0", runs: [{ tool: { driver: { name: "CodeQL" } } }] } }],
  ["a different tool", { "actions.sarif": { version: "2.1.0", runs: [{ tool: { driver: { name: "Other" } }, results: [] }] } }],
]) test(`CodeQL acceptance rejects ${name}`, async t => {
  assert.notEqual(run(await fixture(t, files)).status, 0);
});
