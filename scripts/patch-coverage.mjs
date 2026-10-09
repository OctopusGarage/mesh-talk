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
      file = /^frontend\/src\/.*\.tsx?$/.test(path)
        ? path.slice("frontend/".length)
        : undefined;
    } else if (file && row.startsWith("@@ ")) {
      const hunk = row.match(/\+(\d+)(?:,(\d+))? @@/);
      if (!hunk) throw new Error(`Cannot read diff hunk: ${row}`);
      const start = Number(hunk[1]);
      const count = hunk[2] === undefined ? 1 : Number(hunk[2]);
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
      lines = new Map();
      files.set(relative, lines);
    } else if (row.startsWith("DA:") && lines) {
      const [line, hits] = row.slice(3).split(",", 2).map(Number);
      if (!Number.isInteger(line) || !Number.isFinite(hits))
        throw new Error(`Invalid LCOV line: ${row}`);
      lines.set(line, Math.max(lines.get(line) ?? 0, hits));
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
    for (const line of lines) {
      const hits = reports
        .map((report) => report.get(file)?.get(line))
        .filter((value) => value !== undefined);
      if (hits.length === 0) continue;
      total++;
      if (hits.some((value) => value > 0)) covered++;
      else missing.push({ file, line });
    }
  }
  missing.sort((a, b) => a.file.localeCompare(b.file) || a.line - b.line);
  return { covered, total, missing };
}

async function main() {
  const base = process.env.PATCH_COVERAGE_BASE ?? "origin/main";
  const minimum = Number(process.env.PATCH_COVERAGE_MIN ?? "91");
  if (!Number.isFinite(minimum) || minimum < 0 || minimum > 100)
    throw new Error("PATCH_COVERAGE_MIN must be between 0 and 100");
  const mergeBase = execFileSync("git", ["merge-base", "HEAD", base], {
    encoding: "utf8",
  }).trim();
  const diff = execFileSync(
    "git",
    ["diff", "--unified=0", "--no-ext-diff", mergeBase, "HEAD", "--", "frontend/src"],
    { encoding: "utf8" },
  );
  const paths = process.argv.slice(2);
  if (paths.length === 0)
    throw new Error("Pass at least one LCOV report path");
  const reports = await Promise.all(
    paths.map(async (path) => parseLcov(await readFile(resolve(path), "utf8"))),
  );
  const added = addedSourceLines(diff);
  const result = summarizePatchCoverage(added, reports);
  if (result.total === 0) {
    if (added.size > 0)
      throw new Error("No changed frontend source lines were found in the coverage reports.");
    console.log("No changed frontend source lines to cover.");
    return;
  }
  const percent = (100 * result.covered) / result.total;
  console.log(
    `Frontend patch coverage: ${result.covered}/${result.total} (${percent.toFixed(2)}%); local minimum ${minimum}%.`,
  );
  if (percent < minimum) {
    for (const { file, line } of result.missing)
      console.error(`Uncovered: frontend/${file}:${line}`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  await main();
