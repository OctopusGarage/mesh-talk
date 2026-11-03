/** Inspect actual SARIF; only individually reviewed, unchanged false positives qualify. */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));

/** Bind an existing review to its rule, sink and complete modeled data flow. */
export function findingIdentity(run, result, sourceRoot = repositoryRoot) {
  const uri = location => location?.artifactLocation?.uri ?? run.artifacts?.[location?.artifactLocation?.index]?.location?.uri;
  const sources = new Map();
  const source = location => {
    const name = uri(location);
    const target = path.resolve(sourceRoot, decodeURIComponent(name ?? ""));
    assert.ok(target.startsWith(path.resolve(sourceRoot) + path.sep), "CodeQL finding source escapes the checkout");
    if (!sources.has(target)) {
      try { sources.set(target, readFileSync(target, "utf8").split(/\r?\n/)); }
      catch (error) { throw new Error(`CodeQL finding source evidence unavailable: ${name}`, { cause: error }); }
    }
    const lines = sources.get(target);
    const { startLine, endLine = startLine } = location?.region ?? {};
    assert.ok(Number.isSafeInteger(startLine) && startLine > 0 && Number.isSafeInteger(endLine) && endLine >= startLine && endLine <= lines.length, "CodeQL finding has invalid source span");
    return lines.slice(startLine - 1, endLine).join("\n");
  };
  assert.equal(result.locations?.length, 1, "CodeQL finding must have exactly one reviewed primary location");
  const location = result.locations?.[0]?.physicalLocation;
  const fingerprint = result.partialFingerprints?.primaryLocationLineHash;
  assert.ok(result.ruleId && uri(location) && fingerprint, "CodeQL finding lacks a stable fingerprint");
  assert.ok(result.codeFlows === undefined || Array.isArray(result.codeFlows), "CodeQL finding has malformed data-flow evidence");
  const flows = (result.codeFlows ?? []).map(flow => flow.threadFlows.map(thread => thread.locations.map(step => {
    const physical = step.location?.physicalLocation;
    assert.ok(uri(physical) && step.location?.message?.text, "CodeQL finding has incomplete data-flow evidence");
    return [uri(physical), step.location.message.text, source(physical)];
  }))).map(flow => JSON.stringify(flow)).sort();
  const related = (result.relatedLocations ?? []).map(item => [uri(item.physicalLocation), item.message?.text, source(item.physicalLocation)]).map(item => JSON.stringify(item)).sort();
  return { rule: result.ruleId, path: uri(location), fingerprint, flow: createHash("sha256").update(JSON.stringify([source(location), result.message?.text ?? "", flows, related])).digest("hex") };
}

export function requireCleanCodeQL(directory, reviewed = JSON.parse(readFileSync(new URL("../../.github/codeql/reviewed-findings.json", import.meta.url), "utf8")), sourceRoot = repositoryRoot) {
  assert.ok(typeof directory === "string" && directory.length, "CodeQL SARIF directory is required");
  assert.ok(Array.isArray(reviewed), "CodeQL finding reviews must be an array");
  const approved = new Set(reviewed.map(review => {
    assert.ok(Number.isSafeInteger(review.alert) && review.alert > 0 && review.reason?.length && review.review === `https://github.com/OctopusGarage/mesh-talk/security/code-scanning/${review.alert}`, "CodeQL finding review lacks provenance");
    assert.ok(review.rule && review.path && review.fingerprint && /^[0-9a-f]{64}$/.test(review.flow), "CodeQL finding review lacks exact evidence");
    return JSON.stringify([review.rule, review.path, review.fingerprint, review.flow]);
  }));
  assert.equal(approved.size, reviewed.length, "duplicate CodeQL finding reviews");
  const files = readdirSync(directory, { withFileTypes: true }).filter(entry => entry.isFile() && entry.name.endsWith(".sarif"));
  assert.ok(files.length > 0, "CodeQL produced no SARIF files");
  let runs = 0;
  let reviewedFindings = 0;
  for (const file of files) {
    const report = JSON.parse(readFileSync(path.join(directory, file.name), "utf8"));
    assert.equal(report.version, "2.1.0", `${file.name}: unsupported SARIF version`);
    assert.ok(Array.isArray(report.runs) && report.runs.length > 0, `${file.name}: missing CodeQL runs`);
    for (const run of report.runs) {
      assert.equal(run.tool?.driver?.name, "CodeQL", `${file.name}: unexpected analysis tool`);
      assert.ok(Array.isArray(run.results), `${file.name}: missing CodeQL results`);
      for (const invocation of run.invocations ?? []) {
        assert.notEqual(invocation.executionSuccessful, false, `${file.name}: unsuccessful CodeQL analysis`);
        assert.ok(!(invocation.toolExecutionNotifications ?? []).some(notification => notification.level === "error"), `${file.name}: CodeQL analysis error`);
      }
      for (const result of run.results) {
        const identity = findingIdentity(run, result, sourceRoot);
        assert.ok(approved.has(JSON.stringify([identity.rule, identity.path, identity.fingerprint, identity.flow])), `${file.name}: unreviewed or changed CodeQL finding ${identity.rule} in ${identity.path}`);
        reviewedFindings++;
      }
      runs++;
    }
  }
  return { files: files.length, runs, reviewedFindings };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    assert.equal(process.argv.length, 3, "Usage: codeql-results.mjs SARIF_DIRECTORY");
    const result = requireCleanCodeQL(process.argv[2]);
    console.log(`CodeQL acceptance: ${result.files} SARIF files, ${result.runs} analysis runs, 0 unreviewed findings (${result.reviewedFindings} exact reviewed false positives)`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
