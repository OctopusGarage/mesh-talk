import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const gate = fileURLToPath(new URL("../ci/codeql-results.mjs", import.meta.url));
const clean = () => ({ version: "2.1.0", runs: [{ tool: { driver: { name: "CodeQL" } }, results: [] }] });
async function fixture(t, files) {
  const directory = await mkdtemp(path.join(tmpdir(), "mesh-codeql-results-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  for (const [name, report] of Object.entries(files)) await writeFile(path.join(directory, name), typeof report === "string" ? report : JSON.stringify(report));
  return directory;
}
const run = directory => spawnSync(process.execPath, [gate, directory], { encoding: "utf8", timeout: 5000 });

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
});

test("a completed clean CodeQL analysis passes with explicit zero findings", async t => {
  const result = run(await fixture(t, { "actions.sarif": clean() }));
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /0 findings/);
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
