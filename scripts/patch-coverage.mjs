import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export function addedSourceLines(diff) {
  const added = new Map();
  let file;
  for (const row of diff.split("\n")) {
    if (row.startsWith("+++ ")) {
      const path = row.slice(4).replace(/^b\//, "");
      // Match the unit coverage exclusions; test/declaration changes have no
      // production-line denominator and cannot be measured by these reports.
      file =
        /^frontend\/src\/.*\.tsx?$/.test(path) &&
        !/\.(?:test|d)\.ts$/.test(path)
          ? path.slice("frontend/".length)
          : undefined;
    } else if (file && row.startsWith("@@ ")) {
      const hunk = row.match(/\+(\d+)(?:,(\d+))? @@/);
      if (!hunk) throw new Error(`Cannot read diff hunk: ${row}`);
      const start = Number(hunk[1]);
      const count = hunk[2] === undefined ? 1 : Number(hunk[2]);
      if (count === 0) continue;
      const lines = added.get(file) ?? new Set();
      for (let line = start; line < start + count; line++) lines.add(line);
      added.set(file, lines);
    }
  }
  return added;
}

export function parseLcov(report) {
  const files = new Map();
  let lines;
  for (const row of report.split("\n")) {
    if (row.startsWith("SF:")) {
      const path = row.slice(3).replaceAll("\\", "/");
      const relative = path.includes("/frontend/")
        ? path.split("/frontend/").at(-1)
        : path.replace(/^frontend\//, "");
      lines = files.get(relative) ?? new Map();
      lines.branches ??= new Map();
      files.set(relative, lines);
    } else if (row.startsWith("DA:") && lines) {
      const [line, hits] = row.slice(3).split(",", 2).map(Number);
      if (
        !Number.isInteger(line) ||
        line < 1 ||
        !Number.isInteger(hits) ||
        hits < 0
      )
        throw new Error(`Invalid LCOV line: ${row}`);
      lines.set(line, Math.max(lines.get(line) ?? 0, hits));
    } else if (row.startsWith("BRDA:") && lines) {
      const [lineText, block, branch, taken] = row.slice(5).split(",");
      const line = Number(lineText);
      const hits = taken === "-" ? 0 : Number(taken);
      if (
        !Number.isInteger(line) ||
        line < 1 ||
        !block ||
        !branch ||
        !Number.isInteger(hits)
      )
        throw new Error(`Invalid LCOV branch: ${row}`);
      const branches = lines.branches.get(line) ?? new Map();
      const key = `${block},${branch}`;
      // V8's merged reports can contain negative branch counts. Treat them as
      // uncovered; they must neither reject the whole report nor count as hits.
      branches.set(key, Math.max(branches.get(key) ?? 0, hits));
      lines.branches.set(line, branches);
    } else if (row === "end_of_record") {
      lines = undefined;
    }
  }
  return files;
}

export function summarizePatchCoverage(added, reports) {
  let covered = 0;
  let total = 0;
  const missing = [];
  for (const [file, lines] of added) {
    if (!reports.some((report) => report.has(file)))
      throw new Error(
        `Changed frontend source file missing from coverage reports: frontend/${file}`,
      );
    for (const line of lines) {
      const hits = reports
        .map((report) => report.get(file)?.get(line))
        .filter((value) => value !== undefined);
      if (hits.length === 0) continue;
      total++;
      const branches = new Map();
      for (const report of reports) {
        for (const [key, taken] of report.get(file)?.branches?.get(line) ?? [])
          branches.set(key, Math.max(branches.get(key) ?? 0, taken));
      }
      // Codecov counts partial lines in the denominator, not as covered hits.
      if (
        hits.some((value) => value > 0) &&
        [...branches.values()].every((value) => value > 0)
      )
        covered++;
      else missing.push({ file, line });
    }
  }
  missing.sort((a, b) => a.file.localeCompare(b.file) || a.line - b.line);
  return { covered, total, missing };
}

export function patchCoverageTarget(configuration) {
  // Deliberately accept the maintained explicit numeric policy, not YAML defaults
  // or a second local target. Unsupported policy changes require owner review.
  const rows = configuration
    .split("\n")
    .filter((row) => row.trim() && !row.trimStart().startsWith("#"));
  const path = [];
  let target;
  for (const row of rows) {
    const entry = row.match(/^( *)([\w-]+):(?:\s+(.*?))?\s*$/);
    if (!entry) throw Error("Unsupported codecov.yml mapping syntax");
    const depth = entry[1].length;
    while (path.length && path.at(-1).depth >= depth) path.pop();
    const keys = [...path.map((item) => item.key), entry[2]].join(".");
    if (keys === "coverage.status.patch.default.target") {
      if (target !== undefined || !/^\d+(?:\.\d+)?%?$/.test(entry[3] ?? ""))
        throw Error(
          "Codecov patch target must be one explicit numeric percentage",
        );
      target = Number(entry[3].replace(/%$/, ""));
    }
    path.push({ depth, key: entry[2] });
  }
  if (target === undefined || target < 0 || target > 100)
    throw Error("Missing or invalid codecov.yml patch target");
  return target;
}

function committedFrontendDiff(root, base) {
  const mergeBase = execFileSync("git", ["merge-base", "HEAD", base], {
    encoding: "utf8",
    cwd: root,
  }).trim();
  const diff = execFileSync(
    "git",
    [
      "diff",
      "--unified=0",
      "--no-ext-diff",
      mergeBase,
      "HEAD",
      "--",
      "frontend/src",
    ],
    { encoding: "utf8", cwd: root },
  );
  return diff;
}

const coverageCommand =
  "cd frontend && env FAST=0 node ../scripts/validation/stage.mjs frontend-coverage --profile shared --inputs frontend --artifact coverage/lcov.info -- npm run test:coverage";

export async function prePrPatchCoverage({
  root = process.cwd(),
  base = "origin/main",
  verifyEvidence,
  signal,
} = {}) {
  signal?.throwIfAborted();
  const added = addedSourceLines(committedFrontendDiff(root, base));
  if (!added.size) return { applicable: false };
  const { inputGroup } = await import("./validation/stage-inputs.mjs");
  const verify =
    verifyEvidence ??
    (await import("./validation/stage.mjs")).verifyStageEvidence;
  let proof;
  try {
    proof = await verify({
      root,
      signal,
      env: { ...process.env, FAST: "0" },
      cwd: resolve(root, "frontend"),
      name: "frontend-coverage",
      command: ["npm", "run", "test:coverage"],
      profile: "shared",
      inputs: inputGroup("frontend", "frontend-coverage"),
      artifacts: ["coverage/lcov.info"],
    });
  } catch (error) {
    throw Error(
      `Current frontend coverage evidence is unavailable: ${error.message}. Generate it with: ${coverageCommand}`,
    );
  }
  const bytes = await readFile(resolve(root, "frontend/coverage/lcov.info"));
  signal?.throwIfAborted();
  const expected = proof.artifacts?.find(
    ([name]) => name === "coverage/lcov.info",
  )?.[1];
  if (
    !expected ||
    createHash("sha256").update(bytes).digest("hex") !== expected
  )
    throw Error(
      `Frontend coverage changed after evidence verification. Generate it with: ${coverageCommand}`,
    );
  const result = summarizePatchCoverage(added, [parseLcov(bytes.toString())]);
  if (!result.total)
    return {
      applicable: false,
      reason:
        "Current verified frontend coverage marks no added lines as executable",
    };
  const minimum = patchCoverageTarget(
    await readFile(resolve(root, "codecov.yml"), "utf8"),
  );
  return { ...result, minimum, applicable: true };
}

async function main() {
  const args = process.argv.slice(2);
  if (process.env.PATCH_COVERAGE_MIN !== undefined)
    throw Error(
      "PATCH_COVERAGE_MIN is unsupported; codecov.yml owns the patch target",
    );
  if (args[0] === "--pre-pr") {
    if (args.length !== 2)
      throw Error("Usage: patch-coverage.mjs --pre-pr BASE");
    const controller = new AbortController();
    const cancel = () => controller.abort();
    process.on("SIGINT", cancel);
    process.on("SIGTERM", cancel);
    let result;
    try {
      result = await prePrPatchCoverage({
        base: args[1],
        signal: controller.signal,
      });
    } finally {
      process.removeListener("SIGINT", cancel);
      process.removeListener("SIGTERM", cancel);
    }
    if (!result.applicable) {
      console.log(
        `Frontend patch coverage precheck: not applicable; ${result.reason ?? "no added production source lines"}. Rust and whole-CI coverage remain remote checks.`,
      );
      return;
    }
    reportResult(result, "Frontend verified unit patch coverage precheck");
    return;
  }
  console.log(
    "Diagnostic frontend patch coverage only: raw LCOV source freshness is not verified; this is not the Rust or whole-CI Codecov gate.",
  );
  const base = process.env.PATCH_COVERAGE_BASE ?? "origin/main";
  const minimum = patchCoverageTarget(
    await readFile(resolve("codecov.yml"), "utf8"),
  );
  if (!args.length) throw Error("Pass at least one LCOV report path");
  const reports = await Promise.all(
    args.map(async (path) => parseLcov(await readFile(resolve(path), "utf8"))),
  );
  const added = addedSourceLines(committedFrontendDiff(process.cwd(), base));
  const result = summarizePatchCoverage(added, reports);
  if (result.total === 0) {
    if (added.size > 0)
      throw new Error(
        "No changed frontend source lines were found in the coverage reports.",
      );
    console.log(
      "Frontend patch coverage: not applicable; no added production source lines to cover.",
    );
    return;
  }
  reportResult({ ...result, minimum }, "Frontend patch coverage");
}

function reportResult(result, label) {
  const { minimum } = result;
  const percent = (100 * result.covered) / result.total;
  console.log(
    `${label}: ${result.covered}/${result.total} (${percent.toFixed(2)}%); local minimum ${minimum}%.`,
  );
  if (percent < minimum) {
    for (const { file, line } of result.missing)
      console.error(`Uncovered: frontend/${file}:${line}`);
    process.exitCode = 1;
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
)
  await main();
