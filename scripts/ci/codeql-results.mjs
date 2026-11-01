/** Analysis upload success is not zero-finding acceptance. Inspect the actual SARIF. */
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

export function requireCleanCodeQL(directory) {
  assert.ok(typeof directory === "string" && directory.length, "CodeQL SARIF directory is required");
  const files = readdirSync(directory, { withFileTypes: true }).filter(entry => entry.isFile() && entry.name.endsWith(".sarif"));
  assert.ok(files.length > 0, "CodeQL produced no SARIF files");
  let runs = 0;
  for (const file of files) {
    const report = JSON.parse(readFileSync(path.join(directory, file.name), "utf8"));
    assert.equal(report.version, "2.1.0", `${file.name}: unsupported SARIF version`);
    assert.ok(Array.isArray(report.runs) && report.runs.length > 0, `${file.name}: missing CodeQL runs`);
    for (const run of report.runs) {
      assert.equal(run.tool?.driver?.name, "CodeQL", `${file.name}: unexpected analysis tool`);
      assert.ok(Array.isArray(run.results), `${file.name}: missing CodeQL results`);
      assert.equal(run.results.length, 0, `${file.name}: ${run.results.length} CodeQL findings`);
      runs++;
    }
  }
  return { files: files.length, runs };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    assert.equal(process.argv.length, 3, "Usage: codeql-results.mjs SARIF_DIRECTORY");
    const result = requireCleanCodeQL(process.argv[2]);
    console.log(`CodeQL acceptance: ${result.files} SARIF files, ${result.runs} analysis runs, 0 findings`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
