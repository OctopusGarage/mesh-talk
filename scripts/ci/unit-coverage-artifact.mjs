// Same-workflow reuse only: download-artifact must use the current run, and the
// consumer must match its source, workflow, run and rerun attempt before reuse.
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const [mode, input, output] = process.argv.slice(2);
if (!["seal", "restore"].includes(mode) || !input || !output) {
  throw new Error("Usage: unit-coverage-artifact.mjs seal|restore <input> <output>");
}
const identity = {
  schemaVersion: 1,
  sourceSha: process.env.GITHUB_SHA,
  workflow: process.env.GITHUB_WORKFLOW,
  runId: process.env.GITHUB_RUN_ID,
  runAttempt: process.env.GITHUB_RUN_ATTEMPT,
  platform: process.platform,
  arch: process.arch,
  nodeMajor: process.versions.node.split(".")[0],
  command: "npm run test:coverage",
};
if (!/^[a-f0-9]{40}$/.test(identity.sourceSha || "") || !identity.workflow ||
    !/^[1-9]\d*$/.test(identity.runId || "") || !/^[1-9]\d*$/.test(identity.runAttempt || "")) {
  throw new Error("Complete GitHub source and run identity is required for unit coverage reuse");
}
const lcov = await readFile(mode === "seal" ? input : resolve(input, "lcov.info"));
const sha256 = createHash("sha256").update(lcov).digest("hex");
if (!/^SF:.+$/m.test(lcov.toString()) || !/^DA:[1-9]\d*,\d+/m.test(lcov.toString())) {
  throw new Error("Unit coverage has no measured source lines");
}
if (mode === "seal") {
  await mkdir(output, { recursive: true });
  await writeFile(resolve(output, "lcov.info"), lcov);
  await writeFile(resolve(output, "provenance.json"), `${JSON.stringify({ ...identity, sha256 }, null, 2)}\n`);
} else {
  const manifest = JSON.parse(await readFile(resolve(input, "provenance.json"), "utf8"));
  for (const [key, value] of Object.entries({ ...identity, sha256 })) {
    if (manifest?.[key] !== value) throw new Error(`Coverage provenance mismatch: ${key}`);
  }
  // Validate everything before touching a previous consumer output.
  await mkdir(dirname(resolve(output)), { recursive: true });
  await writeFile(output, lcov);
}
